# bluechip-osmosis-contract

A decentralized subscription and creator economy protocol, built with
CosmWasm for deployment on **Osmosis**. Creator pools pair against the
chain's native asset (**OSMO**, `uosmo`); the commit threshold is
USD-denominated and valued through Osmosis's chain-native `x/twap`
module — no keepers, no external price feeds, no bespoke oracle.

## Overview

Bluechip enables content creators to launch their own tokens and build
portable, decentralized subscription communities. Unlike traditional
subscription platforms where audiences are locked to a single platform,
Bluechip lets creators take their community anywhere while subscribers
earn tokens proportional to their support.

**Decentralized subscriptions** — subscription transactions (on-chain
"commits") are recorded on chain, not controlled by any central
platform. Creators own their subscriber relationships directly, and the
same subscription contract can back any number of websites and apps.

**Portable communities** — creators can integrate the subscribe button
into any website or platform; the community follows the creator, not
the platform.

**Subscriber token rewards** — committers receive creator tokens
proportional to their USD support when the pool launches, becoming
tokenholders in the creator's success. Tokens can be provided as
liquidity to earn trading fees.

---

## Architecture

Three production contracts and two shared library packages:

```
┌─────────────────────────────────────────────────────────────┐
│                      FACTORY CONTRACT                        │
│  - Creates creator pools (permissionless: flat OSMO fee +    │
│    1h/address rate limit)                                    │
│  - Global configuration behind a 48h timelock; every config  │
│    change live-probes the pricing route before it can land   │
│  - USD pricing: stateless x/twap query over the configured   │
│    OSMO/USDC pool (ConvertNativeToUsd)                       │
│  - Pool registry (PoolByAddress / Pools enumeration)         │
│  - Batched, timelocked pool code upgrades                    │
└─────────────────────────────────────────────────────────────┘
        │ creates
        ▼
┌────────────────────┐
│   CREATOR POOL     │
│  - Commit phase    │
│    (OSMO in, USD-  │
│    valued ledger)  │
│  - Threshold cross │
│    mints + seeds   │
│    the AMM         │
│  - Post-threshold  │
│    AMM + commits   │
│  - Batched token   │
│    distribution    │
└────────────────────┘
        │
        ▼
┌─────────────────────────────────────────────────────────────┐
│              POOL-CORE  (shared library package)             │
│  - Constant-product AMM math + slippage / spread guards      │
│  - Position-NFT helpers (deposit, add, remove, collect fees) │
│  - First-depositor MINIMUM_LIQUIDITY inflation lock          │
│  - Reentrancy lock shared across every hot path              │
│  - Auto-pause when reserves drop below MINIMUM_LIQUIDITY     │
│  - Two-phase emergency withdraw (config-set timelock)        │
│  - Strict per-asset fund collection (no orphaned coins)      │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                      ROUTER CONTRACT                         │
│  - Multi-hop swaps (≤3 hops) across registered pools         │
│  - Every hop validated against the factory registry before   │
│    funds move; minimum_receive is the binding slippage gate  │
└─────────────────────────────────────────────────────────────┘
```

(`pool-factory-interfaces` carries the wire-format types the pools,
factory, and router speak; `easy-addr` is a test-only helper.)

---

## How It Works

### Creating a creator pool

Creators call the factory's `Create`. Only the pair shape and CW20
metadata are caller-supplied; every other knob (threshold, fee splits,
payout amounts, lock caps, pricing config) is read from factory config.
The CW20 address is filled in by the factory during the reply chain.

```json
{
  "create": {
    "pool_msg": {
      "pool_token_info": [
        { "bluechip": { "denom": "uosmo" } },
        { "creator_token": { "contract_addr": "WILL_BE_CREATED_BY_FACTORY" } }
      ]
    },
    "token_info": { "name": "Creator Token Name", "symbol": "TICKER", "decimal": 6 }
  }
}
```

**Funds attached:** exactly one coin entry of `uosmo`, amount ≥ the
flat creation fee (`pool_creation_fee`, factory config —
mainnet default 1 OSMO). `cw_utils::must_pay` rejects any other shape;
surplus is refunded in the same tx. The fee goes to the protocol
wallet; a 1-hour per-address rate limit keeps registry spam in check.

---

## Two-Phase Pool Lifecycle

### Phase 1: Pre-threshold (funding)

Before the pool reaches its USD threshold ($25,000 default), only
**commits** are allowed — no swaps, no liquidity operations. Every
commit is valued in USD at entry via the factory's x/twap query and
recorded in a ledger; fees (1% protocol + 5% creator) are split off
first, and the remainder accrues toward the threshold.

### Threshold crossing

When total committed USD reaches the threshold, one atomic transaction:

1. **Creator tokens minted** — 1,200,000 total (see
   [Token Economics](#token-economics))
2. **Creator reward**: 325,000 tokens to the creator's wallet
3. **Protocol reward**: 25,000 tokens to the protocol wallet
   (live-resolved from the factory, so a wallet rotation applies to
   every existing pool)
4. **Pool seeded**: 350,000 tokens + the raised OSMO initialize the AMM
5. **Committer distribution queued**: 500,000 tokens to committers
   pro-rata by USD (paid out in batches — see
   [Batched Distribution](#batched-threshold-distribution))
6. **Excess handling**: OSMO above `max_bluechip_lock_per_pool` goes to
   a time-locked creator escrow
   (see [Creator Limits](#creator-limits--excess-liquidity))
7. **NFT auto-accept**: the pool accepts its position-NFT contract in
   the same tx — no pending-ownership window
8. **Factory notified** (`NotifyThresholdCrossed`, one-shot,
   deferred-on-error with permissionless retry)
9. Pool transitions to active trading

The crossing commit itself is bounded: at most **3% of pool reserves**
can be swapped as excess by the crossing transaction, and anything
beyond that is **refunded** to the committer.

### Phase 2: Post-threshold (active trading)

- **Commits** still work (still 6% fee, still subscription-tracked) —
  they are routed through the AMM and the committer receives creator
  tokens at market price
- **Swaps**, **add/remove liquidity**, **collect fees** all open

A 2-block cooldown delays the first swap after crossing, then a
100-block per-tx swap-cap ramp (0.5% of the offer-side reserve at the
start, linear to unrestricted) bounds early MEV on the freshly seeded
pool.

---

## The Commit Function (Subscribe Button)

```json
{
  "commit": {
    "asset": {
      "info": { "bluechip": { "denom": "uosmo" } },
      "amount": "1000000"
    },
    "transaction_deadline": null,
    "belief_price": null,
    "max_spread": null
  }
}
```

**Send with:** OSMO attached in the same amount as `asset.amount`
(`must_pay`-strict — wrong denom or amount fails fast).

### What happens when you commit

**Pre-threshold:** the OSMO is valued in USD via the factory's x/twap
query (one rate captured at entry, threaded through the whole tx — no
mid-tx drift), the 6% fee is split off, the commitment is recorded in
the ledger, and if the threshold is crossed the payout above triggers
atomically.

**Post-threshold:** 6% fee is split off and the remainder is swapped
through the AMM (subject to the ramp cap); the committer receives
creator tokens.

**If the price can't be fetched** (misconfigured pricing pool, zero or
absurd TWAP), the commit **reverts** — the protocol fails closed rather
than mispricing. Commits are also floored: minimum $5 pre-threshold,
$1 post-threshold (admin-tunable up to $1,000).

**Rate limiting:** 13 seconds minimum between commits per wallet.

### Fee structure

| Fee | Recipient | Amount | When |
|-----|-----------|--------|------|
| Protocol fee | Protocol wallet (live-resolved) | 1% | Commits only |
| Creator fee | Creator wallet | 5% | Commits only |
| LP fee | Liquidity providers | 0.3% | All swaps |

Regular swaps pay only the LP fee. The protocol takes no cut of swaps.

---

## USD Pricing (Osmosis x/twap)

The commit threshold is USD-denominated but commits are paid in OSMO,
so the factory must know the OSMO/USD price. It gets it with a single
stateless chain query: the **arithmetic TWAP of a configured OSMO/USDC
pool** (`pricing_pool_id`) over the last `twap_window_seconds`
(default 600s, bounds 300–3600s), via Osmosis's `x/twap` module.

- **No keeper, no push liveness, nothing to go stale** — the chain
  computes the average at query time from real trading activity.
- **Manipulation cost** = moving the pricing pool's price for the whole
  window. Point `pricing_pool_id` at the deepest OSMO/USDC pool on the
  chain.
- **Fail-closed everywhere**: a query error, a zero/dust price, or a
  price above the **$10,000-per-OSMO sanity ceiling** (`RATE_MAX` —
  which catches wrong-decimals quote denoms and spiked pools) makes the
  valuation revert, so a commit that cannot be priced correctly cannot
  be priced at all.
- **Misconfiguration cannot land**: instantiate, `ProposeConfigUpdate`,
  and `UpdateConfig` all run a **live probe** of the candidate pricing
  route and refuse configs whose TWAP query fails.

Integrators can read the same conversion the pools use:

```json
{ "pool_factory_query": { "convert_native_to_usd": { "amount": "1000000" } } }
```

which returns `{ amount, rate_used, timestamp }` — also the recommended
uptime canary (see `RUNBOOK.md`).

---

## NFT Liquidity Positions

Liquidity positions are represented as NFTs (via `pool-core`):

- **Fee collection without burning** — claim accumulated fees while
  keeping the position
- **Transferable positions** — the NFT is the position
- **Partial withdrawals** — remove some liquidity, keep the NFT

### First-depositor inflation lock

The first deposit on an empty pool locks `MINIMUM_LIQUIDITY = 1000` LP
units into the position (unwithdrawable; still earns fees), and both
credited sides must be ≥ the floor — neutralizing donate-then-deposit
share-price inflation and one-sided dust seeding.

### Adding liquidity

```json
{
  "deposit_liquidity": {
    "amount0": "1000000", "amount1": "1000000",
    "min_amount0": "990000", "min_amount1": "990000",
    "transaction_deadline": null
  }
}
```

Returns the position NFT. CW20-side deposits are verified by a
`reply_on_success` SubMsg asserting `post − pre == credited` against
the token's reported balance.

`add_to_position` tops up an existing position (auto-collecting
pending fees first); `collect_fees { position_id }` claims fees using
the fee-growth checkpoint accounting:

```
fees_owed = (fee_growth_global − fee_growth_at_last_collection) × position_liquidity
```

Small positions are subject to a fee-size multiplier; the clipped
portion routes to the creator fee pot rather than being lost.

`remove_partial_liquidity` / `remove_partial_liquidity_by_percent` /
`remove_all_liquidity` share one handler: partial keeps the NFT, full
burns it. Removals that would drop reserves below `MINIMUM_LIQUIDITY`
auto-pause the pool; the flag clears itself when a deposit restores the
floor.

---

## Query Endpoints

```json
{ "pool_state": {} }
```
`PoolStateResponse`: `nft_ownership_accepted`, `reserve0`, `reserve1`,
`total_liquidity`, `block_time_last`.

```json
{ "is_fully_commited": {} }
```
`"fully_committed"` or `{ "in_progress": { "raised": "...", "target": "25000000000" } }`.

```json
{ "position": { "position_id": "123" } }
```
Plus `positions { start_after, limit }` and
`positions_by_owner { owner, start_after, limit }`.

```json
{ "simulation": { "offer_asset": { "info": { "bluechip": { "denom": "uosmo" } }, "amount": "1000000" } } }
```
Quotes from the same tracked reserves the execute path trades against;
`reverse_simulation { ask_asset }` solves the other direction. Both
return a clean error (not a panic) on a pre-threshold/zero-reserve pool.

```json
{ "analytics": {} }
```
Snapshot for indexers: TVL, fee reserves, threshold status, position
count, swap/commit counters, spot prices both directions.

```json
{ "committing_info": { "wallet": "osmo1..." } }
```
`last_commited { wallet }` (accepts the corrected `last_committed`
spelling too) returns the wallet's most recent commit;
`pool_commits { ... }` pages the full committer ledger.

```json
{ "creator_earnings": {} }
```
Creator-pool only: creator wallet, claimable fee pot, locked excess (+
`claimable_now`), `is_threshold_hit`, `threshold_crossed_at`.

**Factory:** `{ "pools": { "start_after": null, "limit": 30 } }` pages
the registry (max 100/page) — each entry has `pool_id`, `pool_addr`,
and `pool_token_info`. `pool_by_address { pool_addr }` is the
authoritative single lookup the router itself uses.

---

## Integration Guide

### Embedding the commit button

```javascript
// CosmJS
const amount = "1000000"; // uosmo micro-units
const msg = {
  commit: {
    asset: { info: { bluechip: { denom: "uosmo" } }, amount },
    transaction_deadline: null,
    belief_price: null,
    max_spread: null
  }
};
await client.execute(sender, poolAddress, msg, "auto", undefined,
  [{ denom: "uosmo", amount }]);
```

### Depositing liquidity (CW20 approval required)

```javascript
await client.execute(sender, cw20Address, {
  increase_allowance: { spender: poolAddress, amount: "1000000" }
}, "auto");

await client.execute(sender, poolAddress, {
  deposit_liquidity: {
    amount0: "1000000", amount1: "1000000",
    min_amount0: null, min_amount1: null, transaction_deadline: null
  }
}, "auto", undefined, [{ denom: "uosmo", amount: "1000000" }]);
```

### Creator token branding

The factory instantiates every creator token with cw20-base `marketing`
set and the **pool creator as marketing admin** (omitting it at
instantiate would lock branding forever). Creators run
`update_marketing` / `upload_logo` on their token; explorers read
`marketing_info {}` / `download_logo {}`. Treat marketing strings and
logo URLs as **untrusted, creator-controlled display data** — sanitize
before rendering.

---

## Security Considerations

Highlights of the defenses built into the contracts:

### Reentrancy & funds handling
- Single shared `REENTRANCY_LOCK` across commit, swap, and every
  liquidity path; checks-effects-interactions ordering on all
  fund-moving paths; checked/`Uint256` arithmetic throughout.
- Strict per-asset fund collection — deposits reject any attached coin
  whose denom isn't one of the pool's configured sides.

### Pricing security
- Fail-closed x/twap valuation with zero/dust rejection and the
  `RATE_MAX` ($10k/OSMO) sanity ceiling; TWAP window floor 300s.
- Live probe of any proposed pricing config at instantiate / propose /
  apply — a typo'd pool id cannot brick commits.
- Residual operator duty: **monitor the pricing pool's liquidity**
  (x/twap never reports "stale"; a draining pool silently lowers
  manipulation cost — see `RUNBOOK.md`).

### Threshold mechanics
- Crossing is one-shot behind four independent gates (pool dispatcher
  latch, two handler entry gates, factory idempotency flag).
- Ledger conservation: distribution sum ≤ threshold; payout components
  are validated against the pinned canonical amounts; the CW20 cap
  equals the exact payout total, so over-mint fails closed at cw20-base.
- 3% excess-swap cap + refund on the crossing tx; 2-block cooldown and
  100-block swap-cap ramp after crossing.

### CW20 surface
Creator pools only ever pair OSMO against the vanilla cw20-base token
the factory itself mints, so no third-party CW20 code runs inside any
pool. CW20-side deposits are additionally balance-verified by a
`reply_on_success` SubMsg that reverts on any credited/actual mismatch.

### Rate limits & spam
- 13s per-wallet commit/swap cooldown; independent cooldown map for
  liquidity ops (a hostile CW20 cannot stamp an LP's withdrawal
  cooldown).
- 1h per-address pool-creation limits + flat creation fee.

### Router
- Every hop's pool address is validated against the factory registry —
  and its declared (offer, ask) against the pool's real sides — before
  any funds move. `minimum_receive = 0` is rejected; per-hop
  `max_spread` is pinned to the pools' 5% hard cap so `minimum_receive`
  is the binding end-to-end slippage control.

### Admin & governance
- Every privileged factory entry point is admin-gated; all config /
  pool-config / upgrade flows are 48h propose→apply with no
  early-apply or replay, and proposals cannot silently overwrite a
  pending one.
- Two-phase emergency withdraw (config-set delay, 24h mainnet default);
  drains route to the protocol wallet, never the factory.
- Migrate handlers refuse semver downgrades. Put the admin and
  migration keys behind a multisig — `docs/MULTISIG.md`.

---

## Token Economics

Each creator pool mints **1,200,000** creator tokens at threshold
crossing — and the CW20 mint cap is set to exactly this total, so
nothing beyond it can ever be minted:

| Recipient | Amount | % | Purpose |
|-----------|--------|---|---------|
| Committers | 500,000 | ~41.7% | Pro-rata by USD committed |
| Creator | 325,000 | ~27.1% | Creator reward (unlocked at crossing) |
| Protocol wallet | 25,000 | ~2.1% | Protocol sustainability |
| Pool liquidity seed | 350,000 | ~29.2% | Initial AMM liquidity |

```
Commit (1000 OSMO)
   ├── 1%  (10)  → protocol wallet
   ├── 5%  (50)  → creator wallet
   └── 94% (940) → ledger (pre-threshold) / AMM swap (post-threshold)
```

Note: the creator allocation is **not vested** and creators may commit
to their own pools; weigh that when deciding pool parameters.

---

## Creator Limits & Excess Liquidity

`max_bluechip_lock_per_pool` (factory config) caps how much of the
raised OSMO is locked into the AMM at crossing. Anything above the cap
— plus proportional creator tokens — goes into a `CreatorExcessLiquidity`
escrow that unlocks after `creator_excess_liquidity_lock_days`
(default 7):

```json
{ "claim_creator_excess_liquidity": {} }
```

Creator-only, after unlock, once. Tokens go directly to the creator's
wallet.

---

## Batched Threshold Distribution

The crossing transaction **queues** the 500k committer distribution; it
pays nobody directly. Payouts flush in gas-budgeted batches (≤40
recipients/tx, gas-adaptive) via the **permissionless**
`ContinueDistribution` call until the ledger drains:

```
user_tokens = (user_usd / total_usd) × 500,000
```

There is **no keeper bounty** — the protocol operates its own keeper
(`keepers/`, `npm run distribution-keeper`; see `RUNBOOK.md`). A 5s
per-address cooldown applies. If a distribution stalls (24h timeout or
repeated failures) the factory admin can `RecoverPoolStuckStates`;
after 7 days anyone can `self_recover_distribution`.

---

## Admin Operations

All through the factory, all admin-gated, all 48h propose→apply:

- **Factory config**: `ProposeConfigUpdate` → 48h → `UpdateConfig`
  (validation runs at both ends, including the live TWAP probe of the
  pricing route; a pending proposal must be cancelled before it can be
  replaced).
- **Per-pool config**: `ProposePoolConfigUpdate { pool_id, ... }` →
  48h → `ExecutePoolConfigUpdate`.
- **Pool code upgrades**: `UpgradePools { new_code_id, pool_ids, ... }`
  → 48h → execute; batches of ≤10 with `ContinuePoolUpgrade`, skipping
  paused pools.
- **Pause/unpause** individual pools (admin pauses are distinct from
  the reserve auto-pause, which clears itself).
- **Emergency withdraw**: two-phase with the config-set delay;
  cancellable during phase 1.
- **Migration**: factory / creator-pool export migrate
  entry points with semver downgrade protection. (The router has no
  migrate entry point — redeploy to change it.)

---

## Key Constants & Limits

Production defaults. 🧪 = shortened under
`--features integration_short_timing` (never shipped; CI enforces).

| Parameter | Value | Description |
|-----------|-------|-------------|
| Commit threshold (USD) | $25,000 (`25000000000`, 6-dec) | Config; USD value to activate a creator pool |
| Total mint at crossing | 1,200,000 tokens | = CW20 mint cap (over-mint impossible) |
| Min commit (pre / post) | $5 / $1 | Admin-tunable, ceiling $1,000 |
| Commit fees | 1% protocol + 5% creator | Commits only |
| LP swap fee | 0.3% default (0.1%–10% bounds) | All swaps, to LPs (+ clip to creator pot) |
| Excess swap cap at crossing | 3% of reserves | Overshoot beyond it is refunded |
| Post-threshold cooldown / ramp | 2 blocks / 100 blocks | Per-tx swap cap ramps 0.5% → 100% |
| Default / max slippage | 0.5% / 5% (10% with `allow_high_max_spread`) | Swap spread guards |
| Commit & swap rate limit | 13 s per wallet | Separate map for liquidity ops |
| First-depositor lock | 1000 LP units, both sides ≥ floor | `MINIMUM_LIQUIDITY` |
| Distribution batch | ≤40 recipients/tx, 5 s caller cooldown | Permissionless `ContinueDistribution` |
| Distribution stall / public recovery | 24 h / 7 days | Admin recover / anyone recover |
| Pool-creation rate limit 🧪 | 3600 s per address | Both pool kinds |
| Creation fee | flat, config (`1000000` = 1 OSMO) | To protocol wallet, surplus refunded |
| Max OSMO lock per pool | config (mainnet env: 25,000 OSMO) | Excess → creator escrow |
| Creator excess lock | 7 days (config) | Then claimable once |
| TWAP window | 600 s default, bounds 300–3600 s | x/twap lookback |
| Rate sanity ceiling | $10,000 per OSMO (`RATE_MAX`) | Wrong-decimals / spike guard |
| Admin timelock 🧪 | 48 h | All propose→apply flows |
| Emergency-withdraw delay | config 60 s – 7 d (mainnet 24 h) | Phase 1 → Phase 2 |
| Creator token decimals | 6 (enforced) | Matches payout base units |

---

## Development

### Building

```bash
make optimize-all   # cosmwasm/optimizer 0.16.0 → artifacts/*.wasm
make check          # cosmwasm-check each artifact
```

The factory declares two optimizer variants: `factory-prod.wasm` (no
features — **the only deployable artifact**, copied to
`artifacts/factory.wasm`) and `factory-integration.wasm`
(`integration_short_timing`, shortened timelocks for shell tests —
never ship). The Makefile hard-fails rather than leave a stale
`factory.wasm`, and CI's `prod-artifact-guard` enforces feature-clean
prod builds.

### Testing

```bash
cargo test --workspace          # 377 tests
cargo clippy --workspace --tests -- -D warnings
cargo fmt --all -- --check
```

Current suite: creator-pool 218, factory 103, pool-core 34,
router 22. `fuzz/` carries cargo-fuzz math targets; see
`FUZZING.md` for status and the planned property-harness work.

### Repository layout

```
bluechip-osmosis-contract/
├── factory/                  # Factory (registry, config, x/twap pricing)
├── creator-pool/             # Commit phase + AMM
├── router/                   # Multi-hop swap router
├── packages/
│   ├── pool-core/            # Shared AMM library
│   ├── pool-factory-interfaces/  # Shared wire-format types
│   └── easy-addr/            # Test-only address helper
├── fuzz/                     # cargo-fuzz targets (excluded from workspace)
├── keepers/                  # Distribution keeper (the one off-chain bot)
├── frontend/                 # Reference UI
├── ci/                       # Prod-build feature guard
├── docs/                     # OSMOSIS_DEPLOY.md, MULTISIG.md, ...
├── deploy_osmosis.sh         # Store + instantiate + verify, testnet & mainnet
├── osmo_testnet.env          # Testnet deploy config
└── osmosis_mainnet.env       # Mainnet deploy config (governance-gated)
```

### Deployment

```bash
# testnet rehearsal (permissionless uploads; faucet OSMO)
./deploy_osmosis.sh osmo_testnet.env

# mainnet (wasm uploads are governance-gated — see docs/OSMOSIS_DEPLOY.md)
./deploy_osmosis.sh osmosis_mainnet.env
```

The script stores the five wasms (or reuses governance-passed code IDs),
instantiates factory + router, then verifies the deploy by reading the
config back and probing `ConvertNativeToUsd` — you see the live TWAP
rate before calling it done. Operations (the distribution keeper, the
pricing canary, monitoring, governance hygiene) are covered in
`RUNBOOK.md`; multisig setup in `docs/MULTISIG.md`.

---

## Links

- Website: https://www.bluechip.link/home
- Discord: https://discord.gg/gfdWgHFY
- Twitter: https://x.com/BlueChipCreate
