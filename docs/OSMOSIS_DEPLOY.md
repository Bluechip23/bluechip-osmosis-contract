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
  constant-product curve), created and seeded by the pool contract via
  `MsgCreateBalancerPool`. It is **not** created at threshold crossing:
  committers are paid first (batched `ContinueDistribution`), and the
  call after the last batch mints the creator / protocol / pool-seed
  tokens and creates the pool, so nothing is tradable before every
  committer has their tokens. Third-party LP is
  chain-native `MsgJoinPool` / `MsgExitPool` against that pool — the
  contract is not in the LP path (no position NFTs; it only exposes
  `{"native_pool_id":{}}` for discovery).
- **Swaps route through `x/poolmanager`** (`MsgSwapExactAmountIn`)
  with a slippage floor derived from the on-chain estimate and the
  caller's `belief_price`.
- The commit threshold is **OSMO-denominated**: a commit's value
  toward the threshold is simply its attached OSMO. No oracle prices the
  threshold. The protocol's one price read is a chain-native `x/twap`
  query over `pricing_pool_id`, used solely to budget the ~20 USDC GAMM
  pool-creation fee swap at threshold crossing (the fee coin is bought
  there and held until the pool is created) — and it runs inside every
  pre-threshold commit, fail-closed, so `pricing_pool_id` is a liveness
  dependency for ledger-phase commits and crossings (see the parameter
  cheat-sheet below).

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
executes the real `tokenfactory` / `gamm` / `poolmanager` modules
in-process (including the real `x/twap` module for the fee-route TWAP)
and covers create → cross → distribute → pool opens (native pool seed) →
swap → third-party `MsgJoinPool`/`MsgExitPool`:

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
create a commit pool, small commit (raise accounting), cross the
threshold, drain the distribution (TokenFactory payout — its final call
opens the pool), verify the native GAMM pool seeded, swap both
directions, join + exit the native
pool, and route through the router (see `scripts/README.md`). Drop
`COMMIT_THRESHOLD_LIMIT_NATIVE` to a few hundred OSMO in
`osmo_testnet.env` so a crossing is cheap to trigger (it must stay
above the 115-OSMO pre-threshold minimum commit). The testnet
`PRICING_POOL_ID` must point at a real OSMO/stable pool with at
least 10 minutes of TWAP history.

### 4. Governance proposal (draft)

Post to the forum first, then submit. Draft skeleton for the
address-permission route:

> **Title:** Grant <PROJECT> the ability to upload CosmWasm contracts
> on Osmosis
>
> **Summary:** <PROJECT> is a creator-token launchpad built on
> Osmosis-native modules. Creators launch a token as a TokenFactory
> denom paired against OSMO in a two-phase pool. Supporters commit
> OSMO; when a pool's cumulative committed OSMO crosses its fixed
> OSMO-denominated threshold (no price feed values a commit) it
> distributes tokens to committers pro-rata, and only once every
> committer has been paid mints the rest of the fixed 1.2M-token supply
> and seeds a native GAMM balancer pool.
> Once the pool is open it is a standard Osmosis GAMM market: swaps
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
> Test suite: unit/property tests plus an osmosis-test-tube end-to-end
> harness that exercises the real tokenfactory/gamm/poolmanager/twap
> modules. State plainly what independent review the code has and has
> not had; do not describe internal reviews as an audit.
>
> **What this protocol does NOT do:** no external price feeds, no
> keeper-updated oracles, no USD conversion of commits (the threshold is
> OSMO-denominated), no privileged mint of OSMO. Pools hold OSMO, the
> TokenFactory denom they administer, their GAMM LP shares, and — between
> crossing and pool creation — the GAMM creation-fee coin.
>
> **Be exact about admin powers and dependencies** (the README's "Who can
> do what, and how fast" table is the reference): config, per-pool
> config, router registration and pool upgrades are behind a 48h
> timelock; pause, unpause and stuck-state recovery are immediate; the
> emergency drain has its own delay (24h default); and the factory's
> chain-level migrate admin is a multisig, which is a signing threshold,
> not a time delay. The seed liquidity is held by the pool contract and
> is recoverable by that emergency drain — do not call it permanently
> locked. The one on-chain price read is the `x/twap` query on
> `pricing_pool_id` that budgets the GAMM creation-fee swap at crossing;
> it runs in every pre-threshold commit and fails closed, so it can
> block funding and crossing (never the size of the threshold). Commits
> are locked in: the only exit is `WithdrawCommit`, which returns a
> committer's net (post-fee) contribution once a pool has gone 18 months
> without crossing.

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
- [ ] Live fee-route probe (the deploy script does this):
      `{"pool_factory_query":{"commit_context":{}}}` returns the live GAMM
      fee coin and a sane `fee_swap_budget_native`.
- [ ] Confirm the contract admin (migration authority) is the protocol
      multisig, not the deploy key. The deploy script sets it from
      `ADMIN_MULTISIG` and refuses a mainnet deploy without one.
- [ ] Confirm `max_bluechip_lock_per_pool` is at least the commit
      threshold, unless creator excess is intended (the deploy script
      enforces this on mainnet).
- [ ] Create one canary commit pool with a small threshold via config —
      or accept the production threshold and let it fill organically.
- [ ] Start the keeper (`keepers/`): one process that runs the
      distribution, retry-notify and prune loops. No oracle keeper
      exists. If it lags, committers wait for their tokens **and the
      pool does not open for trading**, because the pool is created by
      the `ContinueDistribution` call after the last batch. A
      distribution left idle for more than 24h must be restarted by the
      admin (`RecoverPoolStuckStates`) or, after 7 days, by anyone
      (`SelfRecoverDistribution`).

## Parameter cheat-sheet (decide before mainnet)

| Knob | Meaning | Testnet suggestion | Mainnet decision |
|---|---|---|---|
| `COMMIT_THRESHOLD_LIMIT_NATIVE` | OSMO (6-dec base units) a pool must raise to open | 250 OSMO = `250000000` | 500,000 OSMO = `500000000000` |
| `PRICING_POOL_ID` / `FEE_QUOTE_DENOM` | fee-route pool: its 600s TWAP budgets the cross-denom gamm creation-fee swap at crossing (never values a commit). Read fail-closed inside every pre-threshold commit, so this pool is a liveness dependency for ledger-phase commits and crossings; pick the deepest one | pool 314 (uosmo/USDC-ibc) | the deepest OSMO/allUSDC pool on osmosis-1 (verify id + denom) |
| `POOL_CREATION_FEE` | flat uosmo anti-spam fee on Create | 1 OSMO | 1–10 OSMO |
| `GAMM_POOL_CREATION_FEE` (+`_DENOM`) | the fee COIN x/gamm charges when the native pool is created, funded from the 1% commit-fee retention (never the creator); the pool settles against the LIVE fee and, when the denom is the USD quote (osmosis-1: **20 alloyed USDC**), swaps its native retention into the fee coin via the pricing pool at crossing and holds it until the pool is created | 1 OSMO (`uosmo`) | match `osmosisd q poolmanager params` — 20 USDC as of 2026-07 |
| `COMMIT_FEE_BLUECHIP` / `COMMIT_FEE_CREATOR` | per-commit fee split | 1% / 5% | your call |
| `MAX_BLUECHIP_LOCK_PER_POOL` | OSMO cap (base units, **not dollars**) seeded into the native pool; anything above it → time-locked creator excess | = threshold | = threshold (`500000000000`) so the whole net raise and all 350k pool tokens are seeded; the deploy script refuses a lower cap on mainnet unless `ALLOW_CREATOR_EXCESS=1` |
| `PROTOCOL_WALLET` | receives protocol fees, the 25k protocol tokens, and any emergency drain | deploy key | a multisig or treasury is recommended; `osmosis_mainnet.env` currently sets the deployer key |
| `EMERGENCY_WITHDRAW_DELAY_SECONDS` | delay between arming an emergency withdraw and the drain (6h–7d on the prod build); an armed drain must execute within 7 days | 60 (integration build only) | 86400 (24h) |

Token-side numbers are **not** knobs: every creator token mints
exactly 1.2M tokens (`1_200_000_000_000` base units, 6 decimals) split
325k creator / 25k protocol / 350k pool seed / 500k committers —
pinned by `THRESHOLD_PAYOUT_*_BASE_UNITS` and validated at
instantiate. The 500k are minted to committers in batches first; the
other 700k are minted in the transaction that creates the pool.
