# Shipping to Osmosis

The stack (factory + creator-pool + router) deploys to Osmosis with
pools pairing against **OSMO** (`uosmo`). The protocol is fully
chain-native:

- **Creator tokens are TokenFactory denoms** —
  `factory/{pool_addr}/{symbol}` bank coins minted by the pool contract
  (the denom admin). No CW20 contracts. Total supply per token is
  exactly **1.2 million tokens** (`1_200_000_000_000` base units at the
  enforced 6 decimals; bank metadata registers the ticker at exponent
  6, so wallets and explorers display 1,200,000).
- **The AMM is a native GAMM balancer pool** (50/50 weights ⇒ the
  constant-product curve), created and seeded by the pool contract at
  threshold crossing via `MsgCreateBalancerPool`. Third-party LP is
  chain-native `MsgJoinPool` / `MsgExitPool` against that pool — the
  contract is not in the LP path (no position NFTs; it only exposes
  `{"native_pool_id":{}}` for discovery).
- **Swaps route through `x/poolmanager`** (`MsgSwapExactAmountIn`)
  with a slippage floor derived from the on-chain estimate and the
  caller's `belief_price`.
- The commit threshold is **USD-denominated**: commits are made in
  OSMO and valued via Osmosis's chain-native `x/twap` module over the
  configured OSMO/USDC pool (`pricing_pool_id`) — no keepers, no Pyth,
  no bespoke oracle.

One compiled artifact set works on both testnet and mainnet — only the
instantiate config differs.

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

Budget for the governance timeline: deposit (6,000 OSMO for gov v1 as
of 2026-07, refundable if the proposal passes/meets quorum) + ~5-day
voting period, plus time to socialize the proposal on the
[Osmosis forum](https://forum.osmosis.zone) before submission
(expected etiquette — proposals that skip the forum discussion tend to
fail).

Only **three wasms** need governance: factory, creator-pool, router.
The factory config still carries two legacy `cw20_token_contract_id` /
`cw721_nft_contract_id` fields from the pre-TokenFactory design; they
are unused at runtime — point them at any valid code IDs already
stored on the target chain (e.g. the public cw20-base / cw721-base
uploads) rather than shipping those wasms yourself.

## Recommended sequence

### 1. Build reproducible artifacts

```bash
make optimize-all       # cosmwasm/optimizer builds into artifacts/
make check              # cosmwasm-check each artifact
sha256sum artifacts/*.wasm   # hashes go in the gov proposal
```

### 2. Local end-to-end gate (osmosis-test-tube)

Before spending anything on-chain, run the integration harness — it
executes the real `tokenfactory` / `gamm` / `poolmanager` / `twap`
modules in-process and covers create → cross (native pool seed) →
distribute → swap → third-party `MsgJoinPool`/`MsgExitPool`:

```bash
cd integration-tests && cargo +stable test --release -- --test-threads=1
```

(See `integration-tests/README.md` for the toolchain notes.)

### 3. Full rehearsal on osmo-test-5 (permissionless)

```bash
# fund the deploy key from https://faucet.testnet.osmosis.zone/
./deploy_osmosis.sh osmo_testnet.env
scripts/run_lifecycle_test.sh
```

The lifecycle script exercises the whole surface automatically:
create a commit pool, small commit (USD accounting), cross the
threshold, verify the native GAMM pool seeded, drain the distribution
(TokenFactory payout), swap both directions, join + exit the native
pool, and route through the router (see `scripts/README.md`). Drop
`COMMIT_THRESHOLD_LIMIT_USD` to a few hundred dollars (or less) in
`osmo_testnet.env` so a crossing is cheap to trigger. The testnet
`PRICING_POOL_ID` must point at a real OSMO/USD-stable pool with
enough TWAP history to cover the window. Reference run 2026-07-18:
11/11 pass against factory code 13256 / pool 314 pricing.

### 4. Governance proposal (draft)

Post to the forum first, then submit. Draft skeleton for the
address-permission route:

> **Title:** Grant <PROJECT> the ability to upload CosmWasm contracts
> on Osmosis
>
> **Summary:** <PROJECT> is a creator-token launchpad built on
> Osmosis-native modules. Creators launch a token as a TokenFactory
> denom paired against OSMO in a two-phase pool. Supporters commit
> OSMO; when a pool's cumulative committed value crosses its USD
> threshold (valued via the chain's x/twap over the main OSMO/USDC
> pool) it mints the fixed 1.2M-token supply, seeds a native GAMM
> balancer pool, and distributes tokens to committers pro-rata.
> Post-threshold, the pool is a standard Osmosis GAMM market: swaps
> route through x/poolmanager and liquidity is added/removed with
> native MsgJoinPool/MsgExitPool. The protocol consists of three
> contracts (factory, commit pool, multi-hop router) — no custom
> token or LP-position contracts.
>
> This proposal grants the deployer address `osmo1...` upload
> permission so the protocol can deploy and subsequently ship
> timelocked (48h) upgrades through its factory without a proposal per
> wasm.
>
> **Code:** <new repo URL>, commit `<hash>`. Reproducible builds via
> cosmwasm/optimizer 0.16.0; artifact sha256 hashes: <hashes>.
> Test suite: full unit/integration coverage plus an
> osmosis-test-tube end-to-end harness that exercises the real
> tokenfactory/gamm/poolmanager/twap modules; security review docs
> in-repo.
>
> **What this protocol does NOT do:** no external price feeds or
> keeper-updated oracles (USD valuation uses the chain's own x/twap
> module over the main OSMO/USDC pool), no bridged assets, no
> privileged mint of OSMO — pools only hold OSMO + TokenFactory
> denoms they administer, and every admin mutation is behind a 48h
> timelock.

For the per-contract route instead, generate the combined gov v1
proposal (one vote stores the three wasms, gzip-compressed, hashes and
commit stamped into the summary):

```bash
scripts/make_storecode_proposal.sh          # writes gov/storecode_proposal.json
osmosisd tx gov submit-proposal gov/storecode_proposal.json \
  --from deployer --chain-id osmosis-1 --node <mainnet-rpc> \
  --gas auto --gas-adjustment 1.4 --gas-prices 0.025uosmo
```

Re-check the min deposit with `osmosisd query gov params` before
submitting; the script takes an optional deposit override argument.
Replace the `<FORUM-LINK-HERE>` / `<REPO-URL-HERE>` placeholders in
the generated JSON first.

### 5. Mainnet instantiate

After code IDs exist (either route):

```bash
# fill the *_CODE_ID values + PROTOCOL_WALLET in osmosis_mainnet.env
./deploy_osmosis.sh osmosis_mainnet.env
```

Set `CW20_CODE_ID` / `CW721_CODE_ID` in the env to existing audited
code IDs on osmosis-1 (legacy config fields, unused at runtime — do
not upload fresh copies).

### 6. Post-deploy checklist

- [ ] `osmosisd q wasm contract $FACTORY_ADDR` — verify admin + code id.
- [ ] Query `{"factory":{}}` — verify `bluechip_denom == "uosmo"`,
      threshold, fees, wallet address.
- [ ] Live pricing probe (the deploy script does this):
      `ConvertNativeToUsd(1 OSMO)` returns a sane USD rate.
- [ ] Set the contract admin (migration authority) to the protocol
      multisig, not the deploy key: `osmosisd tx wasm set-contract-admin`.
- [ ] Create one canary commit pool with a small threshold via config —
      or accept the production threshold and let it fill organically.
- [ ] Start the keepers (`keepers/`): distribution keeper, retry-notify
      keeper, prune loop. No oracle keeper exists; nothing fails closed
      if keepers lag — distributions just wait.

## Parameter cheat-sheet (decide before mainnet)

| Knob | Meaning | Testnet suggestion | Mainnet decision |
|---|---|---|---|
| `COMMIT_THRESHOLD_LIMIT_USD` | USD (6-dec) a pool must raise to open | $20–$200 | $25,000 = `25000000000` |
| `PRICING_POOL_ID` / `USD_QUOTE_DENOM` | x/twap pricing pool for OSMO→USD | pool 314 (uosmo/USDC-ibc) | the deepest OSMO/USDC pool on osmosis-1 (verify id + denom) |
| `TWAP_WINDOW_SECONDS` | TWAP lookback (manipulation-cost window) | 600 | 600 |
| `POOL_CREATION_FEE` | flat uosmo anti-spam fee on Create | 1 OSMO | 1–10 OSMO |
| `GAMM_POOL_CREATION_FEE` (+`_DENOM`) | the fee COIN x/gamm charges at crossing, funded from the 1% commit-fee retention (never the creator); the pool settles against the LIVE fee and, when the denom is the USD quote (osmosis-1: **20 Noble USDC**), swaps its native retention into the fee coin via the pricing pool | 1 OSMO (`uosmo`) | match `osmosisd q poolmanager params` — 20 USDC as of 2026-07 |
| `COMMIT_FEE_BLUECHIP` / `COMMIT_FEE_CREATOR` | per-commit fee split | 1% / 5% | your call |
| `MAX_BLUECHIP_LOCK_PER_POOL` | OSMO cap locked into the seed; rest → creator excess | = threshold | your call |
| `PROTOCOL_WALLET` | receives protocol fees | deploy key | **multisig** |
| `EMERGENCY_WITHDRAW_DELAY_SECONDS` | drain timelock | 60 | 86400 (24h) |

Token-side numbers are **not** knobs: every creator token mints
exactly 1.2M tokens (`1_200_000_000_000` base units, 6 decimals) split
325k creator / 25k protocol / 350k pool seed / 500k committers —
pinned by `THRESHOLD_PAYOUT_*_BASE_UNITS` and validated at
instantiate.
