// Native Osmosis GAMM liquidity — in-site add/remove, no redirect.
//
// ⚠️ BUILD/TEST LOCALLY BEFORE GOING LIVE. This is fund-moving code written
// without a compile/testnet environment. `npm run build` it and dry-run
// `addLiquidity` / `removeLiquidity` on osmo-test-5 with tiny amounts before
// exposing it to users. The slippage bounds below (token_in_maxs /
// token_out_mins) mean the WORST case of a mispriced tx is a revert or
// spending up to your chosen max, not a silent drain — but verify anyway.
//
// Why hand-rolled protobuf: the app ships only base @cosmjs/* (0.37), which
// has no Osmosis message types, and we deliberately avoid pulling the heavy
// `osmojs` dependency. GAMM's join/exit messages are trivial protobufs, so we
// encode them directly and register them on a SigningStargateClient. If you
// later add `osmojs`, you can swap `gammRegistryTypes` for its generated types
// and delete the encoders here.

import { Registry, type OfflineSigner } from '@cosmjs/proto-signing';
import { SigningStargateClient, defaultRegistryTypes, GasPrice } from '@cosmjs/stargate';

// ---------------------------------------------------------------------------
// Minimal protobuf writer (proto3, only the wire types these messages use).
// ---------------------------------------------------------------------------

class Writer {
    private buf: number[] = [];

    private varint(v: bigint) {
        let n = v;
        while (n > 0x7fn) {
            this.buf.push(Number((n & 0x7fn) | 0x80n));
            n >>= 7n;
        }
        this.buf.push(Number(n));
    }

    private tag(field: number, wire: number) {
        this.varint(BigInt((field << 3) | wire));
    }

    /** proto string field (wire type 2). */
    string(field: number, value: string) {
        if (value === undefined || value === null) return this;
        const bytes = new TextEncoder().encode(value);
        this.tag(field, 2);
        this.varint(BigInt(bytes.length));
        this.buf.push(...bytes);
        return this;
    }

    /** proto uint64 field (wire type 0), value given as a decimal string. */
    uint64(field: number, value: string) {
        this.tag(field, 0);
        this.varint(BigInt(value));
        return this;
    }

    /** embedded/repeated message field (wire type 2). */
    message(field: number, bytes: Uint8Array) {
        this.tag(field, 2);
        this.varint(BigInt(bytes.length));
        this.buf.push(...bytes);
        return this;
    }

    finish(): Uint8Array {
        return Uint8Array.from(this.buf);
    }
}

export interface Coin {
    denom: string;
    amount: string;
}

// cosmos.base.v1beta1.Coin { denom = 1; amount = 2; }
function encodeCoin(c: Coin): Uint8Array {
    return new Writer().string(1, c.denom).string(2, c.amount).finish();
}

// ---------------------------------------------------------------------------
// GAMM messages
// ---------------------------------------------------------------------------

export const MSG_JOIN_POOL = '/osmosis.gamm.v1beta1.MsgJoinPool';
export const MSG_EXIT_POOL = '/osmosis.gamm.v1beta1.MsgExitPool';

// osmosis.gamm.v1beta1.MsgJoinPool
//   sender = 1; pool_id = 2; share_out_amount = 3; token_in_maxs = 4 (repeated Coin)
export interface MsgJoinPool {
    sender: string;
    poolId: string; // uint64 as decimal string
    shareOutAmount: string;
    tokenInMaxs: Coin[];
}

function encodeMsgJoinPool(m: MsgJoinPool): Uint8Array {
    const w = new Writer().string(1, m.sender).uint64(2, m.poolId).string(3, m.shareOutAmount);
    // Coins MUST be denom-sorted (SDK Coins invariant).
    for (const c of sortCoins(m.tokenInMaxs)) w.message(4, encodeCoin(c));
    return w.finish();
}

// osmosis.gamm.v1beta1.MsgExitPool
//   sender = 1; pool_id = 2; share_in_amount = 3; token_out_mins = 4 (repeated Coin)
export interface MsgExitPool {
    sender: string;
    poolId: string;
    shareInAmount: string;
    tokenOutMins: Coin[];
}

function encodeMsgExitPool(m: MsgExitPool): Uint8Array {
    const w = new Writer().string(1, m.sender).uint64(2, m.poolId).string(3, m.shareInAmount);
    for (const c of sortCoins(m.tokenOutMins)) w.message(4, encodeCoin(c));
    return w.finish();
}

function sortCoins(coins: Coin[]): Coin[] {
    return [...coins].sort((a, b) => (a.denom < b.denom ? -1 : a.denom > b.denom ? 1 : 0));
}

// A CosmJS `GeneratedType` only needs `.encode(value).finish()` for signing.
// We stub the rest and register with `as any` — the Registry never calls the
// decode/JSON paths on the outbound signing path.
function generatedType(encode: (m: any) => Uint8Array) {
    return {
        encode: (message: any) => ({ finish: () => encode(message) }),
        decode: () => ({}),
        fromPartial: (m: any) => m,
    };
}

/** Register these on a Registry to sign GAMM join/exit. */
export const gammRegistryTypes: ReadonlyArray<[string, any]> = [
    [MSG_JOIN_POOL, generatedType(encodeMsgJoinPool)],
    [MSG_EXIT_POOL, generatedType(encodeMsgExitPool)],
];

// ---------------------------------------------------------------------------
// Signing client
// ---------------------------------------------------------------------------

/**
 * A SigningStargateClient that can sign GAMM messages, built from the SAME
 * wallet signer the app already uses. `gasPrice` lets `fee: "auto"` work
 * (join/exit are simulated). Osmosis mainnet base fee ≈ 0.025 uosmo.
 */
export async function getGammSigningClient(
    rpc: string,
    signer: OfflineSigner,
    gasPriceStr = '0.03uosmo',
): Promise<SigningStargateClient> {
    const registry = new Registry([...defaultRegistryTypes]);
    for (const [url, type] of gammRegistryTypes) {
        registry.register(url, type as any);
    }
    return SigningStargateClient.connectWithSigner(rpc, signer, {
        registry,
        gasPrice: GasPrice.fromString(gasPriceStr),
    });
}

// ---------------------------------------------------------------------------
// Proportional-join / exit math (linear, exact — no fee/swap approximation).
// All amounts are base-unit BigInts; LP shares are ~1e20-scale so Number is
// NOT safe here.
// ---------------------------------------------------------------------------

const bpsDen = 10_000n;

/** Ceil-div for BigInt. */
function ceilDiv(a: bigint, b: bigint): bigint {
    return (a + b - 1n) / b;
}

export interface JoinQuote {
    /** LP shares this join mints (share_out_amount). */
    shareOut: bigint;
    /** Max in per side AFTER the slippage cushion (token_in_maxs). */
    tokenInMaxs: { osmo: bigint; creator: bigint };
    /** Exact required in per side at current ratio (what you should hold). */
    required: { osmo: bigint; creator: bigint };
}

/**
 * Drive a proportional join with an OSMO amount. Given the pool's total
 * shares `S` and per-side reserves (`resOsmo`, `resCreator`), an OSMO input
 * mints `shareOut = S * osmoIn / resOsmo` shares and requires
 * `ceil(resCreator * shareOut / S)` creator tokens. token_in_maxs get a
 * `slippageBps` cushion so a small ratio drift between quote and execution
 * doesn't revert.
 */
export function quoteJoinByOsmo(
    osmoIn: bigint,
    totalShares: bigint,
    resOsmo: bigint,
    resCreator: bigint,
    slippageBps: bigint,
): JoinQuote {
    if (totalShares <= 0n || resOsmo <= 0n || resCreator <= 0n)
        throw new Error('pool has no liquidity to quote against');
    const shareOut = (totalShares * osmoIn) / resOsmo;
    if (shareOut <= 0n) throw new Error('input too small to mint any LP shares');
    const reqOsmo = ceilDiv(resOsmo * shareOut, totalShares);
    const reqCreator = ceilDiv(resCreator * shareOut, totalShares);
    const cushion = (x: bigint) => (x * (bpsDen + slippageBps)) / bpsDen;
    return {
        shareOut,
        required: { osmo: reqOsmo, creator: reqCreator },
        tokenInMaxs: { osmo: cushion(reqOsmo), creator: cushion(reqCreator) },
    };
}

export interface ExitQuote {
    /** Expected out per side at current ratio. */
    expected: { osmo: bigint; creator: bigint };
    /** Min out per side AFTER the slippage cushion (token_out_mins). */
    tokenOutMins: { osmo: bigint; creator: bigint };
}

/** Exit `shareIn` LP shares: expected out `floor(res_i * shareIn / S)` per side. */
export function quoteExit(
    shareIn: bigint,
    totalShares: bigint,
    resOsmo: bigint,
    resCreator: bigint,
    slippageBps: bigint,
): ExitQuote {
    if (totalShares <= 0n) throw new Error('pool has no shares');
    const expOsmo = (resOsmo * shareIn) / totalShares;
    const expCreator = (resCreator * shareIn) / totalShares;
    const floorSlip = (x: bigint) => (x * (bpsDen - slippageBps)) / bpsDen;
    return {
        expected: { osmo: expOsmo, creator: expCreator },
        tokenOutMins: { osmo: floorSlip(expOsmo), creator: floorSlip(expCreator) },
    };
}

/** Build the MsgJoinPool EncodeObject for signAndBroadcast. */
export function buildJoinMsg(
    sender: string,
    poolId: string,
    quote: JoinQuote,
    osmoDenom: string,
    creatorDenom: string,
) {
    const value: MsgJoinPool = {
        sender,
        poolId,
        shareOutAmount: quote.shareOut.toString(),
        tokenInMaxs: [
            { denom: osmoDenom, amount: quote.tokenInMaxs.osmo.toString() },
            { denom: creatorDenom, amount: quote.tokenInMaxs.creator.toString() },
        ],
    };
    return { typeUrl: MSG_JOIN_POOL, value };
}

/** Build the MsgExitPool EncodeObject for signAndBroadcast. */
export function buildExitMsg(
    sender: string,
    poolId: string,
    shareIn: bigint,
    quote: ExitQuote,
    osmoDenom: string,
    creatorDenom: string,
) {
    const value: MsgExitPool = {
        sender,
        poolId,
        shareInAmount: shareIn.toString(),
        tokenOutMins: [
            { denom: osmoDenom, amount: quote.tokenOutMins.osmo.toString() },
            { denom: creatorDenom, amount: quote.tokenOutMins.creator.toString() },
        ],
    };
    return { typeUrl: MSG_EXIT_POOL, value };
}
