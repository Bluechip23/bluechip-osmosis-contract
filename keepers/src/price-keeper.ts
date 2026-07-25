// ---------------------------------------------------------------------------
// Pyth price keeper — keeps the factory's OSMO/USD Pyth feed fresh on-chain.
//
// WHY THIS EXISTS: Pyth on Osmosis is push/pull-based. Fresh prices always
// exist off-chain at Pyth's Hermes API, but the on-chain Pyth contract only
// stores the last price SOMEONE pushed. Nobody keeps OSMO/USD pushed on
// Osmosis (every feed on the mainnet Pyth contract was ~115 days stale when
// measured), so the factory — which reads the stored price and fails closed
// past `max_pyth_staleness_seconds` — needs a standing keeper to push it.
//
// Each iteration:
//   1. fetch the latest signed OSMO/USD update from Hermes,
//   2. query the Pyth contract's UpdateFee for that update,
//   3. submit `update_price_feeds { data: [...] }` with the fee attached.
//
// Validated live (2026-07): Hermes returns a fresh OSMO/USD price and the
// mainnet Pyth contract quotes a 1-uosmo update fee for it. The message
// shapes below match pyth-sdk-cw.
//
// Run:  npm run price-keeper   (see keepers/.env.example for config)
// Supervise it (systemd Restart=always / k8s). If it lags past the factory's
// staleness gate, commits fail closed — that is fail-safe, not fund-loss,
// but committers are watching.
// ---------------------------------------------------------------------------

import "dotenv/config";
import { z } from "zod";
import { DirectSecp256k1HdWallet } from "@cosmjs/proto-signing";
import { SigningCosmWasmClient } from "@cosmjs/cosmwasm-stargate";
import { GasPrice, type Coin } from "@cosmjs/stargate";
import { log } from "./lib/logger.js";
import { interruptibleSleep } from "./lib/sleep.js";

const nonEmpty = z.string().min(1);

const PriceConfigSchema = z.object({
  RPC_ENDPOINT: nonEmpty,
  CHAIN_ID: nonEmpty,
  BECH32_PREFIX: nonEmpty.default("osmo"),
  GAS_PRICE: nonEmpty.default("0.025uosmo"),
  GAS_DENOM: nonEmpty.default("uosmo"),
  KEEPER_MNEMONIC: nonEmpty,

  // The Pyth CW contract to push to (testnet vs mainnet — see .env.example).
  PYTH_CONTRACT_ADDR: nonEmpty,
  // 64-hex OSMO/USD feed id (no 0x), MUST match the factory's
  // `pyth_native_usd_feed_id`.
  PYTH_NATIVE_USD_FEED_ID: z
    .string()
    .regex(/^[0-9a-fA-F]{64}$/, "must be 64 hex chars, no 0x"),
  // Hermes endpoint. Mainnet-signed prices for mainnet Pyth contracts;
  // point at the appropriate (beta) Hermes when pushing to a testnet Pyth
  // that verifies against a testnet guardian set.
  HERMES_ENDPOINT: nonEmpty.default("https://hermes.pyth.network"),

  // Push cadence, in milliseconds. It must sit inside TWO bounds set by the
  // factory's on-chain gates:
  //   - UPPER: comfortably under `max_pyth_staleness_seconds` (default 300s)
  //     so a couple of missed pushes don't start failing commits closed —
  //     e.g. 60s push vs 300s gate leaves 4 pushes of slack.
  //   - LOWER: comfortably OVER the factory's `MIN_PYTH_AGE_SECONDS` (10s,
  //     the anti-same-block-MEV gate). Pyth returns the LATEST price, so if
  //     the keeper refreshes faster than that floor the stored price never
  //     ages past 10s and EVERY commit reverts "too fresh" — a silent
  //     protocol-wide liveness brick. The 15_000ms floor keeps ≥1 valid
  //     block of separation with margin over the 10s gate.
  PYTH_PUSH_INTERVAL_MS: z
    .string()
    .default("60000")
    .transform((s, ctx) => {
      const n = Number.parseInt(s, 10);
      if (!Number.isInteger(n) || n < 15000) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message:
            "must be an integer >= 15000 (ms): below the factory's 10s MIN_PYTH_AGE gate " +
            "(plus margin) the stored price never ages past 'too fresh' and all commits brick",
        });
        return z.NEVER;
      }
      return n;
    }),
  // Warn (don't stop) when the keeper wallet drops below this many
  // base-denom units of gas.
  MIN_KEEPER_BALANCE: z
    .string()
    .default("1000000")
    .transform((s) => BigInt(s)),
});

type PriceConfig = z.infer<typeof PriceConfigSchema>;

interface HermesLatest {
  binary: { encoding: string; data: string[] };
  parsed?: Array<{ price: { price: string; conf: string; expo: number; publish_time: number } }>;
}

/** Fetch the latest signed update bytes (base64) for the feed from Hermes. */
async function fetchHermesUpdate(
  cfg: PriceConfig,
): Promise<{ data: string[]; price?: { price: string; expo: number; publish_time: number } }> {
  const url =
    `${cfg.HERMES_ENDPOINT.replace(/\/$/, "")}` +
    `/v2/updates/price/latest?ids[]=${cfg.PYTH_NATIVE_USD_FEED_ID}&encoding=base64`;
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`Hermes ${res.status} ${res.statusText} for ${url}`);
  }
  const body = (await res.json()) as HermesLatest;
  if (!body.binary?.data?.length) {
    throw new Error("Hermes returned no update data");
  }
  return { data: body.binary.data, price: body.parsed?.[0]?.price };
}

async function main(): Promise<void> {
  const cfg = PriceConfigSchema.parse(process.env);

  const wallet = await DirectSecp256k1HdWallet.fromMnemonic(cfg.KEEPER_MNEMONIC, {
    prefix: cfg.BECH32_PREFIX,
  });
  const [account] = await wallet.getAccounts();
  if (!account) throw new Error("derived wallet produced no accounts");
  const address = account.address;
  const signer = await SigningCosmWasmClient.connectWithSigner(cfg.RPC_ENDPOINT, wallet, {
    gasPrice: GasPrice.fromString(cfg.GAS_PRICE),
  });

  log.info("pyth price keeper starting", {
    rpc: cfg.RPC_ENDPOINT,
    chain: cfg.CHAIN_ID,
    pyth: cfg.PYTH_CONTRACT_ADDR,
    feed: cfg.PYTH_NATIVE_USD_FEED_ID,
    interval_ms: cfg.PYTH_PUSH_INTERVAL_MS,
    keeper: address,
  });

  let stopped = false;
  const stop = () => {
    stopped = true;
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  while (!stopped) {
    try {
      // 1. Fresh signed update from Hermes.
      const { data, price } = await fetchHermesUpdate(cfg);

      // 2. Update fee the Pyth contract charges for this update.
      const fee = await signer.queryContractSmart(cfg.PYTH_CONTRACT_ADDR, {
        get_update_fee: { vaas: data },
      });
      const feeCoin = fee as Coin;
      const funds: Coin[] =
        feeCoin && BigInt(feeCoin.amount ?? "0") > 0n ? [feeCoin] : [];

      // 3. Push it on-chain.
      const result = await signer.execute(
        address,
        cfg.PYTH_CONTRACT_ADDR,
        { update_price_feeds: { data } },
        "auto",
        undefined,
        funds,
      );

      log.info("pushed OSMO/USD price update", {
        tx: result.transactionHash,
        price: price?.price,
        expo: price?.expo,
        publish_time: price?.publish_time,
        fee: funds[0] ? `${funds[0].amount}${funds[0].denom}` : "0",
      });

      // Best-effort gas-balance warning.
      try {
        const bal = await signer.getBalance(address, cfg.GAS_DENOM);
        if (BigInt(bal.amount) < cfg.MIN_KEEPER_BALANCE) {
          log.warn("keeper balance below threshold — top up soon", {
            address,
            balance: bal.amount,
            threshold: cfg.MIN_KEEPER_BALANCE.toString(),
          });
        }
      } catch (err) {
        log.warn("balance check failed", {
          detail: err instanceof Error ? err.message : String(err),
        });
      }
    } catch (err) {
      // Never crash the loop on a transient Hermes/RPC/tx error — log and
      // retry next tick. A sustained failure is an ops page (the staleness
      // gate will start failing commits closed).
      log.error("price push iteration failed", {
        detail: err instanceof Error ? err.message : String(err),
      });
    }

    await interruptibleSleep(cfg.PYTH_PUSH_INTERVAL_MS, () => stopped);
  }

  signer.disconnect();
  log.info("pyth price keeper stopped");
}

main().catch((err) => {
  log.error("fatal", { detail: err instanceof Error ? err.stack ?? err.message : String(err) });
  process.exit(1);
});
