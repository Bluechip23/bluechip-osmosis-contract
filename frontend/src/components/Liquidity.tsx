// In-site liquidity for the pool's NATIVE Osmosis GAMM pool — users add /
// remove liquidity here without leaving for app.osmosis.zone.
//
// ⚠️ FUND-MOVING CODE, NOT YET COMPILE/TESTNET-VERIFIED. `npm run build` this,
// then dry-run add + remove on osmo-test-5 with tiny amounts before shipping.
// Slippage bounds (token_in_maxs / token_out_mins) cap the downside to "tx
// reverts" or "spent up to your max", not a drain — but verify end-to-end.
//
// How it works: the crossing seeded a normal public GAMM pool; adding
// liquidity is a native MsgJoinPool and removing is MsgExitPool, signed by the
// user's own wallet (see src/lib/osmosisGamm.ts). The contract is NOT in the
// loop — it only tells us the pool id (`native_pool_id` query) and current
// reserves (`pool_state`). A user's LP position is their bank balance of the
// `gamm/pool/{id}` share denom.
import React, { useState } from 'react';
import {
    Card, CardContent, Typography, TextField, Button, Box, Alert, Tabs, Tab, Divider,
} from '@mui/material';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';
import { DEFAULT_CHAIN_CONFIG, getBluechipDenom, getCreatorTokenDenom } from '../types/FrontendTypes';
import {
    getGammSigningClient, quoteJoinByOsmo, quoteExit, buildJoinMsg, buildExitMsg,
} from '../lib/osmosisGamm';

// RPC/REST for the native signing + supply query. Prefer env overrides; fall
// back to the (placeholder) default config — set these for your deployment.
const RPC = import.meta.env.VITE_RPC_ENDPOINT || DEFAULT_CHAIN_CONFIG.rpc;
const REST = import.meta.env.VITE_REST_ENDPOINT || DEFAULT_CHAIN_CONFIG.rest;

interface LiquidityProps {
    client: SigningCosmWasmClient | null;
    address: string;
}

interface PoolData {
    poolId: string;
    lpDenom: string;
    osmoDenom: string;
    creatorDenom: string;
    resOsmo: bigint;      // reserve0 (bluechip / OSMO side)
    resCreator: bigint;   // reserve1 (creator side)
    totalShares: bigint;  // bank total supply of lpDenom
    userLp: bigint;       // caller's LP-share balance
    userOsmo: bigint;
    userCreator: bigint;
}

const toMicro = (v: string): bigint => {
    const n = parseFloat(v);
    if (!isFinite(n) || n <= 0) throw new Error('enter a positive amount');
    return BigInt(Math.floor(n * 1_000_000));
};
const fromMicro = (v: bigint): string => (Number(v) / 1_000_000).toLocaleString(undefined, { maximumFractionDigits: 6 });

const Liquidity = ({ client, address }: LiquidityProps) => {
    const [tab, setTab] = useState(0);
    const [poolAddr, setPoolAddr] = useState('');
    const [osmoAmount, setOsmoAmount] = useState('');
    const [removePercent, setRemovePercent] = useState('100');
    const [slippagePct, setSlippagePct] = useState('1');
    const [status, setStatus] = useState('');
    const [txHash, setTxHash] = useState('');
    const [pool, setPool] = useState<PoolData | null>(null);
    const [loading, setLoading] = useState(false);

    const slippageBps = (): bigint => {
        const p = parseFloat(slippagePct);
        return BigInt(Math.max(0, Math.min(5000, Math.floor((isFinite(p) ? p : 1) * 100))));
    };

    // ---- Load pool state (reads only) --------------------------------------
    const loadPool = async () => {
        if (!client || !poolAddr) { setStatus('Enter a pool contract address'); return; }
        try {
            setLoading(true);
            setStatus('Loading pool…');
            setPool(null);

            const pair = await client.queryContractSmart(poolAddr, { pair: {} });
            const osmoDenom = getBluechipDenom(pair.asset_infos) ?? DEFAULT_CHAIN_CONFIG.nativeDenom;
            const creatorDenom = getCreatorTokenDenom(pair.asset_infos);
            if (!creatorDenom) throw new Error('could not resolve the creator denom from pair {}');

            const npid = await client.queryContractSmart(poolAddr, { native_pool_id: {} });
            if (!npid?.pool_id || !npid?.lp_share_denom) {
                throw new Error('pool has not crossed its threshold yet — no native pool to LP against');
            }
            const poolId: string = String(npid.pool_id);
            const lpDenom: string = npid.lp_share_denom;

            // reserve0 = OSMO side, reserve1 = creator side (fixed pair order).
            const st = await client.queryContractSmart(poolAddr, { pool_state: {} });
            const resOsmo = BigInt(st.reserve0 ?? '0');
            const resCreator = BigInt(st.reserve1 ?? '0');

            // Total LP shares = bank total supply of the gamm/pool/{id} denom.
            const supplyRes = await fetch(
                `${REST}/cosmos/bank/v1beta1/supply/by_denom?denom=${encodeURIComponent(lpDenom)}`,
            );
            const supplyJson = await supplyRes.json();
            const totalShares = BigInt(supplyJson?.amount?.amount ?? '0');

            const [lpBal, osmoBal, creatorBal] = await Promise.all([
                client.getBalance(address, lpDenom),
                client.getBalance(address, osmoDenom),
                client.getBalance(address, creatorDenom),
            ]);

            setPool({
                poolId, lpDenom, osmoDenom, creatorDenom,
                resOsmo, resCreator, totalShares,
                userLp: BigInt(lpBal.amount), userOsmo: BigInt(osmoBal.amount), userCreator: BigInt(creatorBal.amount),
            });
            setStatus('');
        } catch (err) {
            setStatus('Error: ' + (err as Error).message);
        } finally {
            setLoading(false);
        }
    };

    // ---- Native signer (same wallet, GAMM-aware registry) ------------------
    const signAndSend = async (msg: { typeUrl: string; value: unknown }) => {
        if (!client) throw new Error('connect a wallet');
        const keplr = (window as unknown as { keplr?: any }).keplr;
        if (!keplr) throw new Error('Keplr not found');
        const chainId = await client.getChainId();
        await keplr.enable(chainId);
        const signer = keplr.getOfflineSigner(chainId);
        const signingClient = await getGammSigningClient(RPC, signer);
        // fee "auto" simulates the tx (the registry can encode our GAMM msgs).
        const res = await signingClient.signAndBroadcast(address, [msg as any], 'auto');
        return res.transactionHash;
    };

    // ---- Add liquidity (MsgJoinPool) ---------------------------------------
    const handleAdd = async () => {
        if (!pool) { setStatus('Load the pool first'); return; }
        try {
            setLoading(true); setStatus('Adding liquidity…'); setTxHash('');
            const osmoIn = toMicro(osmoAmount);
            const quote = quoteJoinByOsmo(osmoIn, pool.totalShares, pool.resOsmo, pool.resCreator, slippageBps());
            if (quote.required.creator > pool.userCreator) {
                throw new Error(
                    `need ${fromMicro(quote.required.creator)} creator token, you hold ${fromMicro(pool.userCreator)}`,
                );
            }
            if (quote.tokenInMaxs.osmo > pool.userOsmo) {
                throw new Error(`need up to ${fromMicro(quote.tokenInMaxs.osmo)} OSMO (incl. slippage), balance too low`);
            }
            const msg = buildJoinMsg(address, pool.poolId, quote, pool.osmoDenom, pool.creatorDenom);
            const hash = await signAndSend(msg);
            setTxHash(hash);
            setStatus(`Success — added ~${fromMicro(quote.required.osmo)} OSMO + ~${fromMicro(quote.required.creator)} creator, minted ${quote.shareOut.toString()} shares.`);
            await loadPool();
        } catch (err) {
            setStatus('Error: ' + (err as Error).message);
        } finally {
            setLoading(false);
        }
    };

    // ---- Remove liquidity (MsgExitPool) ------------------------------------
    const handleRemove = async () => {
        if (!pool) { setStatus('Load the pool first'); return; }
        try {
            setLoading(true); setStatus('Removing liquidity…'); setTxHash('');
            const pct = Math.max(0, Math.min(100, parseFloat(removePercent) || 0));
            if (pct <= 0) throw new Error('enter a percent between 0 and 100');
            const shareIn = (pool.userLp * BigInt(Math.floor(pct * 100))) / 10_000n;
            if (shareIn <= 0n) throw new Error('you hold no LP shares in this pool');
            const quote = quoteExit(shareIn, pool.totalShares, pool.resOsmo, pool.resCreator, slippageBps());
            const msg = buildExitMsg(address, pool.poolId, shareIn, quote, pool.osmoDenom, pool.creatorDenom);
            const hash = await signAndSend(msg);
            setTxHash(hash);
            setStatus(`Success — withdrew ~${fromMicro(quote.expected.osmo)} OSMO + ~${fromMicro(quote.expected.creator)} creator.`);
            await loadPool();
        } catch (err) {
            setStatus('Error: ' + (err as Error).message);
        } finally {
            setLoading(false);
        }
    };

    return (
        <Card sx={{ mb: 2 }}>
            <CardContent>
                <Typography variant="h6" gutterBottom>Liquidity (native Osmosis pool)</Typography>

                <Box sx={{ display: 'flex', gap: 1, mb: 2 }}>
                    <TextField
                        fullWidth size="small" label="Pool contract address" value={poolAddr}
                        onChange={(e) => setPoolAddr(e.target.value)} placeholder="osmo1…"
                    />
                    <Button variant="outlined" onClick={loadPool} disabled={loading}>Load</Button>
                </Box>

                {pool && (
                    <Box sx={{ mb: 2, fontSize: '0.85rem', color: 'text.secondary' }}>
                        <div>GAMM pool #{pool.poolId} · reserves {fromMicro(pool.resOsmo)} OSMO / {fromMicro(pool.resCreator)} creator</div>
                        <div>Your LP shares: {pool.userLp.toString()} ({pool.totalShares > 0n ? (Number(pool.userLp * 10000n / pool.totalShares) / 100).toFixed(2) : '0'}% of pool)</div>
                        <div>Your balances: {fromMicro(pool.userOsmo)} OSMO · {fromMicro(pool.userCreator)} creator</div>
                    </Box>
                )}

                <Tabs value={tab} onChange={(_, v) => setTab(v)} sx={{ mb: 2 }}>
                    <Tab label="Add" /><Tab label="Remove" />
                </Tabs>

                <TextField
                    fullWidth size="small" label="Max slippage %" value={slippagePct}
                    onChange={(e) => setSlippagePct(e.target.value)} sx={{ mb: 2 }} helperText="e.g. 1 for 1%"
                />

                {tab === 0 && (
                    <Box sx={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                        <TextField
                            label="OSMO to add" value={osmoAmount} type="number"
                            onChange={(e) => setOsmoAmount(e.target.value)}
                            helperText="The matching creator-token amount is computed at the pool's current ratio."
                        />
                        {pool && osmoAmount && (() => {
                            try {
                                const q = quoteJoinByOsmo(toMicro(osmoAmount), pool.totalShares, pool.resOsmo, pool.resCreator, slippageBps());
                                return <Alert severity="info">Pairs with ~{fromMicro(q.required.creator)} creator token · mints {q.shareOut.toString()} LP shares</Alert>;
                            } catch { return null; }
                        })()}
                        <Button variant="contained" onClick={handleAdd} disabled={loading || !pool}>
                            {loading ? 'Processing…' : 'Add liquidity'}
                        </Button>
                    </Box>
                )}

                {tab === 1 && (
                    <Box sx={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                        <TextField
                            label="Percent of your position to remove" value={removePercent} type="number"
                            onChange={(e) => setRemovePercent(e.target.value)} helperText="0–100"
                        />
                        <Button variant="contained" color="secondary" onClick={handleRemove} disabled={loading || !pool}>
                            {loading ? 'Processing…' : 'Remove liquidity'}
                        </Button>
                    </Box>
                )}

                <Divider sx={{ my: 2 }} />
                {status && (
                    <Alert severity={status.startsWith('Success') ? 'success' : status.startsWith('Error') ? 'error' : 'info'}>
                        {status}
                    </Alert>
                )}
                {txHash && (
                    <Typography variant="body2" sx={{ mt: 1, fontFamily: 'monospace', wordBreak: 'break-all' }}>
                        tx: {txHash}
                    </Typography>
                )}
            </CardContent>
        </Card>
    );
};

export default Liquidity;
