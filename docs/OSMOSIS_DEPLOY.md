# Shipping to Osmosis

The stripped-down stack (factory + creator-pool + standard-pool + router)
deploys to Osmosis with pools pairing against **OSMO** (`uosmo`). The
commit threshold is **USD-denominated**: commits are made in OSMO and
valued via Osmosis's chain-native `x/twap` module over the configured
OSMO/USDC pool (`pricing_pool_id`) — no keepers, no Pyth, no bespoke
oracle. One compiled artifact set works on both testnet and mainnet —
only the instantiate config differs.

## The one hard constraint: mainnet uploads are governance-gated

- **osmo-test-5 (testnet): permissionless.** Anyone can `wasm store`.
- **osmosis-1 (mainnet): permissioned.** Contract uploads require
  governance. Two routes:
  1. **Per-contract StoreCode proposals** — one proposal carrying the
     wasm(s). After it passes, the code IDs exist on chain and anyone
     named in the proposal's instantiate permission can instantiate.
  2. **Address-permission proposal** — governance grants your deployer
     address the ability to upload contracts without further proposals
     (this is the route projects like Router Protocol and QSTN took).
     Cleaner if you expect to iterate/migrate contracts over time —
     pool-code upgrades via the factory's `UpgradePools` need new code
     IDs, so route 2 is strongly recommended.

Budget for the governance timeline: deposit (1,600 OSMO, refundable if
the proposal passes/meets quorum) + ~5-day voting period, plus time to
socialize the proposal on the [Osmosis forum](https://forum.osmosis.zone)
before submission (expected etiquette — proposals that skip the forum
discussion tend to fail).

## Recommended sequence

### 1. Build reproducible artifacts

```bash
make optimize-all       # cosmwasm/optimizer builds into artifacts/
make check              # cosmwasm-check each artifact
sha256sum artifacts/*.wasm   # hashes go in the gov proposal
```

### 2. Full rehearsal on osmo-test-5 (permissionless, do this first)

```bash
# fund the "alice" key from https://faucet.testnet.osmosis.zone/
./deploy_osmosis.sh osmo_testnet.env
```

Then run through the whole lifecycle against the testnet factory:
create a commit pool, commit past a (lowered) threshold, verify the
crossing seeds the AMM + starts distribution, swap, add/remove
liquidity, create a standard pool, route a 2-hop swap. Drop
`COMMIT_THRESHOLD_LIMIT_USD` to a few hundred dollars in
`osmo_testnet.env` so a crossing is cheap to trigger. The testnet
`PRICING_POOL_ID` must point at a real OSMO/USD-stable pool with
enough TWAP history to cover the window — create a small one first if
none exists.

### 3. Governance proposal (draft)

Post to the forum first, then submit. Draft skeleton for the
address-permission route:

> **Title:** Grant <PROJECT> the ability to upload CosmWasm contracts
> on Osmosis
>
> **Summary:** <PROJECT> is a creator-token launchpad: creators launch
> a CW20 paired against OSMO in a two-phase pool. Supporters commit
> OSMO; when a pool's cumulative committed value crosses its USD
> threshold (valued via the chain's x/twap over the main OSMO/USDC
> pool) it self-seeds a constant-product AMM and distributes creator
> tokens to committers pro-rata. Post-threshold, the pool is a standard xyk
> market. The protocol consists of four contracts (factory, commit
> pool, standard pool, multi-hop router) plus the audited cw20-base /
> cw721-base contracts for creator tokens and LP position NFTs.
>
> This proposal grants the deployer address `osmo1...` upload
> permission so the protocol can deploy and subsequently ship
> timelocked (48h) upgrades through its factory without a proposal per
> wasm.
>
> **Code:** <new repo URL>, commit `<hash>`. Reproducible builds via
> cosmwasm/optimizer 0.16.0; artifact sha256 hashes: <hashes>.
> Test suite: 458 unit/integration tests; security review docs in-repo.
>
> **What this protocol does NOT do:** no external price feeds or
> keeper-updated oracles (USD valuation uses the chain's own x/twap
> module over the main OSMO/USDC pool), no bridged assets, no
> privileged mint of OSMO — pools only hold OSMO + project-minted
> CW20s, and every admin mutation is behind a 48h timelock.

For the per-contract route instead: `osmosisd tx gov submit-proposal
wasm-store artifacts/factory.wasm --title ... --deposit 1600000000uosmo ...`
(one per wasm, or a combined proposal via the newer submit-proposal JSON
format).

### 4. Mainnet instantiate

After code IDs exist (either route):

```bash
# fill the *_CODE_ID values + PROTOCOL_WALLET in osmosis_mainnet.env
./deploy_osmosis.sh osmosis_mainnet.env
```

Note on cw20/cw721: audited cw20-base and cw721-base code IDs already
exist on osmosis-1 from other projects. Reusing one (verify its code
hash against the published cw-plus / cw-nfts release) keeps your
governance proposal smaller; the factory just takes the code IDs as
instantiate parameters.

### 5. Post-deploy checklist

- [ ] `osmosisd q wasm contract $FACTORY_ADDR` — verify admin + code id.
- [ ] Query `{"factory":{}}` — verify `bluechip_denom == "uosmo"`,
      threshold, fees, wallet address.
- [ ] Set the contract admin (migration authority) to the protocol
      multisig, not the deploy key: `osmosisd tx wasm set-contract-admin`.
- [ ] Create one canary commit pool with a small threshold via config —
      or accept the production threshold and let it fill organically.
- [ ] Start the keepers (`keepers/`): distribution keeper, retry-notify
      keeper, prune loop. No oracle keeper exists anymore; nothing
      fails closed if keepers lag — distributions just wait.

## Parameter cheat-sheet (decide before mainnet)

| Knob | Meaning | Testnet suggestion | Mainnet decision |
|---|---|---|---|
| `COMMIT_THRESHOLD_LIMIT_USD` | USD (6-dec) a pool must raise to open | a few hundred dollars | $25,000 = `25000000000` |
| `PRICING_POOL_ID` / `USD_QUOTE_DENOM` | x/twap pricing pool for OSMO→USD | your own test pool | the deepest OSMO/USDC pool on osmosis-1 (verify id + denom) |
| `TWAP_WINDOW_SECONDS` | TWAP lookback (manipulation-cost window) | 600 | 600 |
| `STANDARD_POOL_CREATION_FEE` | flat uosmo anti-spam fee | 1 OSMO | 1–10 OSMO |
| `COMMIT_FEE_BLUECHIP` / `COMMIT_FEE_CREATOR` | per-commit fee split | 1% / 5% | your call |
| `MAX_BLUECHIP_LOCK_PER_POOL` | OSMO cap locked into reserves; rest → creator excess | = threshold | your call |
| `PROTOCOL_WALLET` | receives protocol fees | deploy key | **multisig** |
| `EMERGENCY_WITHDRAW_DELAY_SECONDS` | drain timelock | 60 | 86400 (24h) |
