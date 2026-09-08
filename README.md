# bluechip-osmosis-contract

A decentralized subscription / creator-economy protocol built with CosmWasm
for **Osmosis**. Creators launch a token by raising a fixed,
**native-denominated** commit threshold in **OSMO**; when the threshold is
crossed the protocol mints the token, seeds a **native Osmosis GAMM pool**,
and airdrops the token to the people who funded it.

The protocol has **no in-house AMM, no CW20, no LP-position NFT, and no
price oracle** — chain-native modules do the heavy lifting:

| Concern | Implementation |
|---|---|
| Creator token | **TokenFactory** denom `factory/{pool}/{sub}` |
| AMM venue | **GAMM** balancer pool (`gamm/pool/{id}`) |
| Swaps | **poolmanager** `MsgSwapExactAmountIn` |
| LP position | the pool contract holds its GAMM LP shares directly |
| Commit threshold | **native OSMO base units** — no price feed values a commit; the only price read is the [fee-route TWAP](#the-fee-route-twap-the-only-price-read) that budgets the GAMM creation-fee swap |

> Reviewing the code? Start with `packages/pool-core/src/osmosis_msgs.rs`
> (every native message the system builds lives there), then
> `creator-pool/src/commit.rs` (the commit dispatcher), then
> `creator-pool/src/commit/threshold_payout.rs` (the crossing).

---

## Contracts

```
factory/          Creates & registers every pool. Owns global config
                  (48h timelock). Serves the per-commit CommitContext
                  (live protocol wallet + GAMM-creation-fee budget).
creator-pool/     One instance per creator. Commit ledger → threshold
                  crossing → post-threshold trading. Denom admin of its
                  own TokenFactory token; holds its GAMM LP shares.
router/           Multi-hop swaps (≤3 hops) across registered pools.
packages/
  pool-core/      Shared library: the Osmosis message builders
                  (osmosis_msgs.rs), swap orchestration + slippage
                  (swap.rs), reentrancy/rate-limit primitives (generic.rs),
                  emergency-withdraw + pause (admin.rs), shared state.
  pool-factory-interfaces/  Wire-format types the three contracts speak.
  easy-addr/      Test-only address helper.
```

`pool-core` contains no AMM math — it is the typed surface for
TokenFactory / GAMM / poolmanager and the swap/reply plumbing.

---

## How OSMO is used

`bluechip_denom` is the chain's native asset, **`uosmo`**, pinned in factory
config and enforced on every pool (`validate_pool_token_info`). OSMO is:

- the **only** asset a commit may attach (`must_pay` strict),
- the unit the **commit threshold is denominated in** — a commit's value
  toward the threshold IS its attached OSMO, with no conversion,
- the **pairing side** of every creator pool (`asset_infos[0]` is always the
  `Native` OSMO side, `asset_infos[1]` the creator TokenFactory side),
- the denom the **GAMM pool-creation-fee reserve** is retained in (the fee
  itself is charged in whatever coin x/poolmanager params name — 20 alloyed
  USDC on osmosis-1 — and the pool swaps its OSMO retention into that coin
  at crossing when they differ; see
  [how the pool is paid for](#how-the-liquidity-pool-is-paid-for)), and
- the reserve the creator's over-cap excess is paid out in.

The commit *threshold* is **500,000 OSMO** at launch
(`commit_threshold_limit_native = 500000000000`, 6-dec base units), tunable
through the factory's 48h config flow (a retune applies to pools created
after it). No oracle prices a commit or the threshold, so no external feed or
thin-liquidity price can change *how much* OSMO a pool needs to cross. The
protocol does make one on-chain price read — the pricing pool's 600s
arithmetic TWAP in `factory/src/fee_twap.rs`, which budgets the ~20 USDC GAMM
creation-fee swap at crossing. It runs inside every **pre-threshold** commit
(via the factory's `CommitContext` query) and fails closed: if the TWAP query
errors, or the price exceeds the plausibility ceiling, pre-threshold commits
and crossings on every pool revert until the pricing pool recovers or the
factory config is re-pointed. Post-threshold commits skip it. See
[the fee-route TWAP](#the-fee-route-twap-the-only-price-read).

---

## Lifecycle

A pool moves through three stages. The **only** user action available before
the threshold is `Commit`; everything else is gated on `IS_THRESHOLD_HIT`.

```
 Stage 0            Stage 1                Stage 2                 Stage 3
 CREATE     →       PRE-THRESHOLD    →     CROSSING (atomic)   →   POST-THRESHOLD
 factory.Create     Commit (funding)       mint + seed GAMM        Commit (buy) + SimpleSwap
                    OSMO-valued ledger     + queue airdrop         + distribution
```

### Stage 0 — Create (factory, permissionless)

Anyone may create a pool (flat OSMO fee + 1h/address rate limit). The creator
supplies only the pair shape and the token's name/symbol/decimals; every
economic knob comes from factory config.

```json
{ "create": {
  "pool_msg": { "pool_token_info": [
    { "bluechip": { "denom": "uosmo" } },
    { "creator_token": { "denom": "WILL_BE_CREATED_BY_FACTORY" } }
  ] },
  "token_info": { "name": "My Creator Token", "symbol": "MYTOKEN", "decimal": 6 }
} }
```

The factory instantiates the pool; the **pool** then creates its own
TokenFactory denom at instantiate and registers bank metadata so explorers
show the creator's chosen name/symbol (not a raw micro-denom):

```rust
// creator-pool/src/contract.rs — instantiate
let creator_denom = pool_core::osmosis_msgs::full_denom(&env.contract.address, &msg.subdenom);
// factory/{pool_addr}/{subdenom}
let create_denom = pool_core::osmosis_msgs::create_denom_msg(&env.contract.address, &msg.subdenom);
// register name/symbol/6-dec display — dispatched reply_on_error so a
// metadata edge case can never revert pool creation (it's display-only).
let set_metadata = SubMsg::reply_on_error(
    pool_core::osmosis_msgs::set_denom_metadata_msg(/* name, symbol, decimals */),
    REPLY_ID_SET_DENOM_METADATA,
);
```

The pool contract is the denom **admin**, which is what lets it mint at
crossing and during distribution. Direct instantiation is rejected — the
caller must be the factory:

```rust
if info.sender != cfg.expected_factory_address { return Err(ContractError::Unauthorized {}); }
```

### Stage 1 — Pre-threshold (funding)

Each `Commit` attaches OSMO, has fees split off, and is recorded in a
ledger. The gross OSMO accrues toward the threshold; the net enters the
pool's balance toward the seed.

```json
{ "commit": {
  "asset": { "info": { "bluechip": { "denom": "uosmo" } }, "amount": "115000000" },
  "transaction_deadline": null, "belief_price": null, "max_spread": null
} }
```

```rust
// creator-pool/src/commit.rs — one factory round-trip for live context
// (protocol wallet + gamm-fee budget). The threshold needs NO valuation:
// a commit's value toward it IS its attached native amount.
let threshold_already_hit = IS_THRESHOLD_HIT.load(deps.storage)?;
let commit_ctx = get_commit_context(
    deps.as_ref(),
    &pool_info.factory_addr,
    !threshold_already_hit,   // post-threshold commits skip the fee budget
)?;
let commit_value = asset.amount;
let live_bluechip_wallet = commit_ctx.bluechip_wallet;   // live, so wallet rotations apply
```

Fees are split for **every** commit path (1% protocol + 5% creator):

```rust
let (commit_fee_bluechip_amt, commit_fee_creator_amt) = calculate_commit_fees(amount, &fee_info)?;
```

The net enters the pool's OSMO balance and the committer is recorded:

```rust
super::record_committer(deps.storage, &sender, commit_value)?;   // ledger + O(1) distinct-committer count
GROSS_NATIVE_COMMITTED.save(deps.storage, &new_total)?;          // GROSS OSMO toward the threshold
NATIVE_RAISED_FROM_COMMIT.update(..)?;                           // NET OSMO held toward the seed
```

Commits are floored (min **115 OSMO** pre / **25 OSMO** post, admin-tunable
per pool up to 25,000 OSMO) and rate-limited to **13s/wallet**.
Pre-threshold, `SimpleSwap` and every claim/recover path reject — only
`Commit` works.

### Stage 2 — Threshold crossing (one atomic transaction)

When a commit pushes `GROSS_NATIVE_COMMITTED` to the target,
`trigger_threshold_payout` runs. It is a **one-shot** event guarded by four
independent gates (dispatcher latch, two handler entry gates, and the
load-bearing `IS_THRESHOLD_HIT` check):

```rust
// creator-pool/src/commit/threshold_payout.rs
if IS_THRESHOLD_HIT.may_load(storage)?.unwrap_or(false) {
    return Err(ContractError::StuckThresholdProcessing);   // never mint/seed twice
}
```

It does five things:

**(1) Mint the four token splits** via TokenFactory `MsgMint` (pool is admin):

```rust
other_msgs.push(mint_tokens(pool, denom, &creator_wallet,  payout.creator_reward_amount));  // 325,000 → creator
other_msgs.push(mint_tokens(pool, denom, bluechip_wallet,  payout.bluechip_reward_amount)); //  25,000 → protocol
other_msgs.push(mint_tokens(pool, denom, &pool_contract,   payout.pool_seed_amount));       // 350,000 → pool (to seed)
// payout.commit_return_amount (500,000) is minted per-committer during distribution.
```

**(2) Queue the committer airdrop** (`DISTRIBUTION_STATE`) — paid in batches
later, not here (see [distribution](#batched-distribution)).

**(3) Seed a native GAMM balancer pool** with the raised OSMO + the pool-seed
tokens. Equal weights give a standard constant-product (`x·y=k`) curve:

```rust
// packages/pool-core/src/osmosis_msgs.rs
const BALANCER_EQUAL_WEIGHT: &str = "1";        // equal weights = 50/50 constant product
let create_pool = SubMsg::reply_on_success(
    create_balancer_pool_msg(&pool_contract, &coin_osmo, &coin_creator, lp_fee),
    REPLY_ID_CREATE_POOL,
);
```

The reply records the new pool id so post-threshold swaps can route:

```rust
// creator-pool/src/contract.rs — reply
REPLY_ID_CREATE_POOL => {
    let pool_id = parse_created_pool_id(&msg.result)?;   // decode MsgCreateBalancerPoolResponse
    POOL_ID.save(deps.storage, &pool_id)?;
}
```

Because the create rides a `reply_on_success` SubMsg, **a failed GAMM
creation reverts the whole crossing** — so if the pool ends up `FullyCommitted`,
the native pool provably exists.

**(4) Handle over-raise** — the OSMO above the cap is escrowed for the creator
(see [excess](#excess-liquidity-above-the-lock-cap)).

**(5) Notify the factory** (`NotifyThresholdCrossed`, one-shot + idempotent).
It's dispatched `reply_on_error` so a factory hiccup can't revert the
crossing; a permissionless `RetryFactoryNotify` re-sends it.

### Stage 3 — Post-threshold (active trading)

- **`SimpleSwap`** routes through the native pool via `MsgSwapExactAmountIn`;
  the output is forwarded to the receiver in the reply.
- **`Commit` still works** — post-threshold it's a market **buy**: the net
  OSMO (after the same 1%+5% fees) is swapped for the creator token. These
  commits pass `include_fee_budget: false` to the factory, so a
  pricing-pool outage can never block trading-phase commits.
- **`ContinueDistribution`** flushes the airdrop in batches.

```rust
// creator-pool/src/commit/post_threshold.rs — the swap leg
let swap_msg = swap_exact_amount_in_msg(&pool_contract, pool_id, &token_in, &creator_denom, token_out_min_amount);
let swap_submsg = SubMsg::reply_on_success(swap_msg, REPLY_ID_SWAP_FORWARD)
    .with_payload(to_json_binary(&payload)?);
```

Third parties can also trade the pool **directly on Osmosis** — it's a normal
native GAMM pool. The contract's `SimpleSwap` is just one convenience venue on
top of it.

---

## Who owns the initial liquidity? (No one.)

At crossing the pool contract creates the GAMM pool and receives its
`gamm/pool/{id}` **LP shares into its own balance**. There is **no
liquidity-deposit or liquidity-withdraw entry point** on the creator pool —
the execute surface is commit / swap / distribution / claims / admin, and
nothing else:

```rust
// creator-pool/src/msg.rs — ExecuteMsg has NO Deposit/Withdraw/RemoveLiquidity variants
SimpleSwap { .. }  Commit { .. }  ContinueDistribution {}  ClaimCreatorExcessLiquidity { .. }
Pause {}  Unpause {}  EmergencyWithdraw {}  RecoverStuckStates { .. }  /* ... */
```

So the seed liquidity is **permanently locked in the pool contract** and
belongs to no user. It cannot be pulled, rugged, or transferred. The only
path that ever moves it is the admin two-phase **emergency withdraw**, which
sweeps the LP shares to the protocol wallet (a break-glass, timelocked
control — not a normal LP exit). Trading fees accrue to the locked position;
nobody can claim them out.

---

## How the liquidity pool is paid for

Creating a GAMM pool costs the chain's `PoolCreationFee` (an
`x/poolmanager` param — **20 alloyed USDC** on osmosis-1,
governance-adjustable), charged *on top of* the seeded coins when
`MsgCreateBalancerPool` executes. Neither the creator nor the committers pay
it directly — it's funded from the **protocol's own 1% commit fee**, retained
in-pool during funding up to a live target:

```rust
// creator-pool/src/commit.rs — only retain toward the fee while
// pre-threshold; once crossed the full 1% always goes to the wallet.
let bluechip_fee_to_wallet = if threshold_already_hit {
    commit_fee_bluechip_amt
} else {
    reserve_bluechip_fee(              // fills BLUECHIP_FEE_RESERVED toward the live target
        deps.storage,
        commit_fee_bluechip_amt,
        gamm_fee_cfg.as_ref(),
        &bluechip_denom,
        &fee_quote_denom,
        fee_swap_budget,
    )?
};
```

At crossing the contract resolves the fee **coin** from the **live chain
param** (not a stale config guess), so a governance change or a mis-set
config can't brick the crossing:

```rust
// creator-pool/src/commit/threshold_payout.rs — cross-denom fee resolution
let fee_coin: Option<Coin> = query_pool_creation_fee_coin(querier) // authoritative x/poolmanager param
    .or_else(|| fee_cfg.cloned())                                  // live factory config (CommitContext)
    .or_else(|| /* instantiate-time native-denom fallback */)
    .filter(|c| !c.amount.is_zero());
```

The fee's **denom** decides how it is paid. On chains that charge it in the
native denom (osmo-test-5: 1 OSMO) the module deducts it straight from the
pool's OSMO balance. On **osmosis-1 the fee is 20 alloyed USDC** — the pool
holds no USDC, so the crossing first emits a `MsgSwapExactAmountOut` through
the factory's pricing pool (which trades OSMO against the fee-quote denom by
definition), converting the retained OSMO reserve into *exactly* the fee
coin; exact-out leaves zero USDC dust. The swap's spend cap is the factory's
[fee-route TWAP](#the-fee-route-twap-the-only-price-read) valuation plus a
20% margin, **hard-clamped to the retained reserve**:

```rust
// creator-pool/src/commit/threshold_payout.rs — HARD CLAMP to the reserve:
// MsgSwapExactAmountOut draws from the pool's ENTIRE bank balance up to
// token_in_max_amount, so an unclamped budget would let a manipulated
// pricing-pool TWAP route committer seed funds out through the fee swap.
let max_in = base_in
    .multiply_ratio(10_000u128 + FEE_SWAP_MARGIN_BPS, 10_000u128)
    .checked_add(Uint128::one())?
    .min(reserved);
```

Clamped, the swap can spend at most the 1%-retention reserve — protocol fee
revenue, never committer or seed funds. Any other fee denom fails with an
actionable config error instead of an opaque gamm revert. Either way the
funding source is the same: **the 1% commit-fee retention — protocol
revenue, never the creator.**

The seed is then sized so `seed_osmo + creation_fee ≤ balance` always holds
(the protocol absorbs any shortfall via a smaller seed, never the creator's
escrow), and any reserve surplus is remitted back to the protocol wallet. If
the fee ever met or exceeded the whole raise, the crossing fails with a clear,
actionable error rather than an opaque gamm revert:

```rust
if seed_osmo.is_zero() {
    return Err(ContractError::InvalidThresholdParams { msg: format!(
        "pool-creation fee ({}) meets or exceeds the raised bluechip seed ({}); \
         the commit threshold is too small relative to the chain's pool-creation fee",
        creation_fee, base_seed_osmo
    ) });
}
```

The whole-tx OSMO conservation invariant
(`seed_osmo + creation_fee + leftover + earmark == raised_net + reserved`) is
pinned by a property test in
`creator-pool/src/testing/invariant_tests.rs`.

---

## Threshold overshoot (the crossing commit is capped)

The crossing commit only counts what's needed to reach the target; the entire
post-fee **excess is refunded** to the committer in the same tx — you cannot
over-raise the recorded total:

```rust
// creator-pool/src/commit/threshold_crossing.rs
let effective_bluechip_excess = amount_after_fees.checked_sub(threshold_portion_after_fees)?;
if !effective_bluechip_excess.is_zero() {
    messages.push(get_bank_transfer_to_msg(&sender, &bluechip_denom, effective_bluechip_excess)?);
}
```

`GROSS_NATIVE_COMMITTED` is pinned to exactly the target at crossing —

```rust
GROSS_NATIVE_COMMITTED.save(deps.storage, &commit_config.commit_amount_for_threshold_native)?;
```

— so the committer ledger provably sums to the threshold and the
500,000-token airdrop can never over-mint.

---

## Excess liquidity above the lock cap

`max_bluechip_lock_per_pool` caps how much of the raised OSMO is seeded into
the GAMM pool, so a single AMM never has to absorb the full raise. Anything
above the cap — plus the proportional creator tokens — is **time-locked to
the creator**, not seeded and not lost:

```rust
// creator-pool/src/commit/threshold_payout.rs
if pools_bluechip_seed > commit_config.max_bluechip_lock_per_pool {
    let excess_bluechip = pools_bluechip_seed.checked_sub(commit_config.max_bluechip_lock_per_pool)?;
    let excess_creator_tokens = payout.pool_seed_amount.multiply_ratio(excess_bluechip, pools_bluechip_seed);
    CREATOR_EXCESS_POSITION.save(storage, &CreatorExcessLiquidity {
        creator: fee_info.creator_wallet_address.clone(),
        bluechip_amount: excess_bluechip,           // RAW OSMO, kept in the contract
        token_amount:    excess_creator_tokens,     // RAW creator tokens, minted-but-not-seeded
        unlock_time: env.block.time.plus_seconds(
            commit_config.creator_excess_liquidity_lock_days * SECONDS_PER_DAY),
    })?;
    // pool is seeded with max_bluechip_lock_per_pool OSMO + the non-earmarked creator tokens.
}
```

The earmarked coins **stay in the contract's bank balance** and the creator
claims them once, after `unlock_time`:

```json
{ "claim_creator_excess_liquidity": { "transaction_deadline": null } }
```

The claim is creator-only, one-shot, and — deliberately — **survives an
emergency drain** (the earmark is the creator's own coins, so the drain
excludes it via `saturating_sub`).

---

## Sandwich / MEV protection

Every native swap carries a `token_out_min_amount` floor, derived as the more
protective of two independent floors:

```rust
// packages/pool-core/src/swap.rs
// token_out_min = max( estimate_floor , belief_floor )
```

- **`belief_floor`** = `(offer / belief_price) · (1 − max_spread)`, from a
  caller-supplied `belief_price` (an off-chain quote the attacker can't move).
  This is the real anti-sandwich guard: it's fixed at submit time, so a
  front-run that moves the pool makes the swap **revert** instead of filling
  at the worse price.
- **`estimate_floor`** = `estimated_out · (1 − max_spread)`, from the
  poolmanager quote at current state. This is a **liveness / zero-quote
  guard, NOT anti-sandwich** — it's computed against the (possibly already
  front-run) pool, so on its own it only stops dispatching against a stale or
  zero quote.

Because the estimate floor is not sandwich-resistant, **post-threshold
commits require an explicit `belief_price`** (there's no end-to-end
`minimum_receive` backstop on that path, unlike the router):

```rust
// creator-pool/src/commit/post_threshold.rs
if belief_price.is_none() {
    return Err(ContractError::BeliefPriceRequired {});
}
```

The reference frontend takes a live `Simulation` quote at submit time and sets
`belief_price = offer / expected_out`. A direct `SimpleSwap` with
`belief_price: null` is likewise rejected — with one exception: the
**registered router**, verified against the live factory registration, may
swap null-belief because it pins each hop's `max_spread` to the 5% cap and
enforces an end-to-end `minimum_receive` instead (a `minimum_receive` of 0 is
rejected). The same registration also exempts the router from the per-address
13s swap cooldown — every hop of a route arrives at the pool as the router,
so all router users would otherwise share one rate-limit slot per pool.
Direct callers keep the cooldown.

**Post-crossing circuit breaker.** Before any contract-routed swap or
post-threshold commit, a relative liquidity breaker compares the live GAMM
pool to what was seeded and **reverts that transaction** if either side is
below 25% of its seed (a drain signal). It is a per-transaction check, not a
latched pause: nothing is written, the caller's funds come back through
normal tx failure, and the next transaction re-evaluates the live pool on its
own. There is no auto-pause for the multisig to reset, and the committer
distribution (`ContinueDistribution`) is never blocked by it. Only an explicit
admin `Pause` halts the pool.

```rust
// packages/pool-core/src/swap.rs — Err(LiquidityBelowSeedFloor) reverts the
// swap; no pause flag is touched.
enforce_liquidity_breaker(storage, querier, pool_id, bluechip_denom, creator_denom)?;
```

---

## Other protections

- **Fail-closed fee budgeting** — the only price read in the protocol is the
  [fee-route TWAP](#the-fee-route-twap-the-only-price-read), and it fails
  closed: a query error, a zero or unparseable price, or a price above the
  plausibility ceiling reverts the pre-threshold commit; the route is
  **live-probed** at factory instantiate and at every config propose/apply.
  Post-threshold commits skip the budget entirely, so a pricing-pool outage
  can never block trading.
- **Reentrancy** — one shared `REENTRANCY_LOCK` wraps commit and swap;
  checked/`Uint256` arithmetic throughout; `overflow-checks = true` in release.
- **Strict fund handling** — `must_pay` on every commit/swap rejects
  multi-denom or wrong-amount attachments, so no stray coins are ever
  absorbed.
- **One-shot crossing** — four independent gates + the factory idempotency
  flag; `NotifyThresholdCrossed` is callable only by the registered pool,
  once.
- **Distribution isolation** — each per-committer mint is a `reply_always`
  SubMsg; a single failing recipient lands in `FAILED_MINTS` (claimable via
  `ClaimFailedDistribution`) instead of reverting the batch. Stalls recover
  via admin (`RecoverPoolStuckStates`, 1h) or anyone
  (`SelfRecoverDistribution`, 7d).
- **Rate limits & spam** — 13s per-wallet commit/swap cooldown (registered
  router exempt); 5s per-caller `ContinueDistribution` cooldown; 1h/address
  pool-creation limit + flat OSMO creation fee.
- **Admin & governance** — every privileged factory entry point is
  admin-gated; all config / pool-config / upgrade flows are **48h
  propose→apply** with no early-apply and no silent overwrite of a pending
  proposal; every pending change is publicly readable via the factory
  `PendingChanges {}` query, and an apply **expires 7 days** after its
  timelock elapses (re-propose required); `bluechip_denom` is **immutable**
  on a live factory; two-phase emergency withdraw (config-set delay, 6h–7d
  bounds, 24h default) routes to the protocol wallet; `migrate` refuses
  semver downgrades and foreign storage (cw2 name mismatch). Put
  admin/migration keys behind a multisig — mainnet deploys refuse to run
  without one.
- **Router** — every hop's pool is validated against the factory registry (and
  its declared pair against the pool's real sides) before funds move; a route
  through a pre-threshold pool is rejected up front.

---

## The fee-route TWAP (the only price read)

**No oracle prices the threshold** — it is native-denominated, so no commit
is ever "valued". The protocol does have exactly one price read, in
`factory/src/fee_twap.rs`, and it prices exactly one thing: the native budget
for the ~$20 cross-denom GAMM creation-fee swap at threshold crossing. It is a
chain-module read (`x/twap` on the factory's `pricing_pool_id`), so there is
no keeper and no external feed — but it IS a live dependency: every
pre-threshold commit executes it through the factory's `CommitContext` query,
so a pricing-pool outage blocks ledger-phase commits and crossings (not
trading) until the pool recovers or the multisig re-points
`pricing_pool_id`:

```rust
// factory/src/fee_twap.rs — arithmetic TWAP over the trailing 600s window.
let resp = TwapQuerier::new(&deps.querier)
    .arithmetic_twap_to_now(
        config.pricing_pool_id,
        // base = fee denom, quote = native → price is native per fee unit.
        config.fee_quote_denom.clone(),
        config.bluechip_denom.clone(),
        Some(osmosis_std::shim::Timestamp { seconds: start as i64, nanos: 0 }),
    )?;
```

Every failure **fails closed** — a pre-threshold commit that cannot budget
the fee safely reverts rather than mis-budgeting:

| Gate | Rule |
|---|---|
| Query error | propagates; the commit reverts |
| Zero / unparseable price | rejected |
| Plausibility ceiling | price > 1,000 uosmo per fee unit rejected (`MAX_NATIVE_PER_FEE_UNIT` — OSMO at $0.001, ~40x below spot, so real moves have huge headroom) |
| Rounding | budget = **ceil**(fee × price) — never undershoots by a base unit |
| Window | 600s trailing (`FEE_TWAP_WINDOW_SECONDS`) |

**Why a TWAP is safe here.** The blast radius is the fee, not the raise. A
manipulated TWAP either (a) under-budgets — the exact-out swap exceeds its
`token_in_max`, the whole crossing reverts atomically, and it is retried
later (liveness only, funds safe) — or (b) over-budgets — the pool side
hard-clamps the swap's spend to its 1%-retention reserve
(`BLUECHIP_FEE_RESERVED`), so the worst case spends protocol fee revenue,
never committer or seed funds. Manipulating a 600s TWAP costs more than
either outcome is worth.

The factory sizes the budget against **max(configured fee, live
x/poolmanager fee)** so config drift or a chain-governance fee change can
never strand a crossing, and serves it via the same `CommitContext` query
every commit already makes. Post-threshold commits pass
`include_fee_budget: false` — they never fund a fee swap, so a pricing-pool
outage cannot block trading. The route is live-probed at factory
**instantiate** and at every config **propose/apply**, so a dead or
misconfigured route is refused before it can matter.

Integrators read the same context the pools use:

```json
{ "pool_factory_query": { "commit_context": {} } }
// → { "timestamp", "bluechip_wallet", "gamm_pool_creation_fee",
//     "fee_swap_budget_native", "pricing_pool_id", "fee_quote_denom" }
```

> The factory's `pricing_pool_id` / `fee_quote_denom` are **not** a price
> source for the threshold. They exist only as the cross-denom fee-swap
> route used at threshold crossing to acquire a USDC-denominated GAMM
> creation fee.

---

## Token economics

Exactly **1,200,000** creator tokens (6-dec ⇒ `1_200_000_000_000` base units)
are minted at crossing — the split is validated against canonical constants at
config, instantiate, and runtime, so nothing else can ever be minted from the
threshold payout:

| Recipient | Tokens | % | Notes |
|---|---|---|---|
| Committers | 500,000 | ~41.7% | pro-rata by OSMO committed; airdropped in batches |
| Creator | 325,000 | ~27.1% | unlocked at crossing (not vested) |
| Protocol wallet | 25,000 | ~2.1% | live-resolved recipient |
| Pool seed | 350,000 | ~29.2% | seeds the GAMM pool (owned by no user) |

```
Commit (1000 OSMO)
  ├─ 1%  (10)  → protocol wallet
  ├─ 5%  (50)  → creator wallet
  └─ 94% (940) → ledger (pre-threshold)  |  AMM buy (post-threshold)
```

Committer reward: `user_tokens = (user_osmo / total_osmo) × 500,000`; floor-
division dust is settled to the creator on the final batch. Creator tokens are
**not vested** and creators may commit to their own pools — weigh that when
choosing pool parameters.

---

## Batched distribution

The crossing **queues** the 500k airdrop; it pays nobody directly. The
protocol keeper (`keepers/`, `npm run distribution-keeper`) — the protocol's
**only** standing off-chain job — calls the permissionless
`ContinueDistribution` until the ledger drains (≤40 recipients/tx,
gas-adaptive, 5s per-caller cooldown). There is no keeper bounty.
Termination is driven by ledger-emptiness, so no extra cleanup call is ever
needed. Nothing fails closed if the keeper lags — distributions simply wait
until someone (anyone) calls.

```json
{ "continue_distribution": {} }
```

---

## Key queries

```json
{ "is_fully_commited": {} }
// "fully_committed"  |  { "in_progress": { "raised": "...", "target": "500000000000" } }

{ "simulation": { "offer_asset": { "info": { "bluechip": { "denom": "uosmo" } }, "amount": "1000000" } } }
// { return_amount, spread_amount, commission_amount } — quoted from the native pool

{ "creator_earnings": {} }
// creator wallet, locked excess (+ claimable_now), is_threshold_hit, threshold_crossed_at

{ "distribution_state": {} }        // live airdrop cursor + is_stalled
{ "analytics": {} }                 // volume/commit/swap counters for indexers
```

Factory: `{ "pools": { "start_after": null, "limit": 30 } }` pages the registry;
`{ "pool_by_address": { "pool_addr": "osmo1..." } }` is the authoritative
lookup the router uses; `{ "creator_token_info": { "pool_id": 1 } }` returns
the denom + on-chain total supply; `{ "pending_changes": {} }` exposes every
pending timelocked config / router / upgrade proposal.

---

## Key constants

| Parameter | Value | Where |
|---|---|---|
| Commit threshold | 500,000 OSMO (`500000000000`, 6-dec native base units; expected to be retuned over time via the 48h config flow — applies to pools created after the change) | factory config |
| Total mint at crossing | 1,200,000 (325k/25k/350k/500k) | `THRESHOLD_PAYOUT_*_BASE_UNITS` |
| Commit fees | 1% protocol + 5% creator | commits only |
| GAMM swap fee (LP) | 0.3% default (0.1%–10% bounds) | `DEFAULT_LP_FEE` → pool `swap_fee` |
| Min commit (pre / post) | 115 OSMO / 25 OSMO (ceiling 25,000 OSMO) | `DEFAULT_MIN_COMMIT_NATIVE_*` |
| Circuit-breaker floor | 25% of seeded per-side | `BREAKER_FLOOR_PERCENT` |
| Commit/swap rate limit | 13 s / wallet (registered router exempt) | `DEFAULT_SWAP_RATE_LIMIT_SECS` |
| Max OSMO lock per pool | config (excess → creator escrow) | `max_bluechip_lock_per_pool` |
| Creator excess lock | 7 days (config), then claim once | `CreatorExcessLiquidity.unlock_time` |
| Fee-TWAP window | 600 s trailing | `FEE_TWAP_WINDOW_SECONDS` |
| Fee-TWAP plausibility ceiling | 1,000 uosmo / fee base unit | `MAX_NATIVE_PER_FEE_UNIT` |
| GAMM creation fee | live x/poolmanager param (osmosis-1: 20 alloyed USDC; swapped from the 1%-retention reserve at crossing, spend clamped to that reserve) | funded from the 1% reserve |
| Admin timelock | 48 h (all propose→apply); apply expires 7 d after the timelock | `ADMIN_TIMELOCK_SECONDS` / `ADMIN_APPLY_WINDOW_SECONDS` |
| Emergency-withdraw delay | 6 h – 7 d (24 h default) | factory config |
| Distribution batch | ≤40 / tx; admin recover 1h / public 7d | `MAX_DISTRIBUTIONS_PER_TX` |
| Creator token decimals | 6 (enforced) | `validate_creator_token_info` |

---

## How this is validated

Two layers, both runnable from this repo:

**300 workspace unit/property tests** (mock chain): every fee split, gate,
recovery path, and the crossing-conservation property test
(`creator-pool/src/testing/invariant_tests.rs`).

**An 8-test end-to-end suite on the real chain modules**
(`integration-tests/`, osmosis-test-tube): the contracts run against an
in-process Osmosis chain binary — real tokenfactory, gamm, poolmanager, and
twap modules, not mocks. It covers:

1. **Factory instantiate against a live TWAP route** — the fee-route probe
   accepts a working pricing pool and refuses a dead one.
2. **Full lifecycle** — create → commit → cross → swap, with the real
   `MsgCreateBalancerPool`, reply protobuf decode, and TokenFactory mints.
3. **Mainnet-shape crossing** — the pool pays a 20-USDC creation fee by
   swapping its OSMO retention through a live pricing pool at crossing.
4. **TWAP orientation** — a 25:1-price pricing pool proves the fee budget is
   quoted native-per-fee-unit, not the inverse.
5. **Third-party LP join/exit** — outside LPs join and exit the native pool;
   the seed position stays locked.
6. **Mid-crossing atomicity** — an injected module failure mid-crossing
   reverts mints, ledger, fees, and funds in full.
7. **Unroutable fee fail-closed** — an unroutable live fee denom bricks the
   crossing with an actionable error until the route is restored, then
   recovers.
8. **Router end-to-end** — registration timelock, belief-price gate, and the
   no-wedge path.

---

## Development

```bash
cargo test --workspace                      # unit + property tests (mock chain)
cargo clippy --workspace --tests -- -D warnings
cargo fmt --all -- --check
make optimize-all                           # deterministic wasm → artifacts/*.wasm
```

Current suite: **creator-pool 160, factory 108, router 24, pool-core 8**
(includes the crossing-conservation property test). Ship the factory **`prod`**
optimizer build only (real 48h timelocks); the `integration_short_timing`
build is for shell tests and is CI-guarded against shipping.

### Osmosis integration tests (`integration-tests/`)

An **excluded** crate runs the contracts against a real in-process Osmosis
chain via `osmosis-test-tube`, exercising what mocks can't (real
`MsgCreateBalancerPool`, the reply protobuf decode, TokenFactory mints,
`MsgSwapExactAmountIn`, and live `x/twap` reads). It needs a chain-capable
toolchain + built wasm — see `integration-tests/README.md`. It is not built
by a normal `cargo test`.

### Layout

```
factory/  creator-pool/  router/
packages/{pool-core, pool-factory-interfaces, easy-addr}
integration-tests/   # osmosis-test-tube e2e (excluded from workspace)
keepers/             # off-chain bot: distribution keeper
frontend/            # reference UI
docs/                # OSMOSIS_DEPLOY.md, FRONTEND_MIGRATION.md
deploy_osmosis.sh    # store + instantiate + verify (testnet & mainnet)
```

### Deploy

```bash
./deploy_osmosis.sh osmo_testnet.env       # testnet
./deploy_osmosis.sh osmosis_mainnet.env    # mainnet (wasm uploads governance-gated)
```

The script stores the wasms, instantiates factory + router, then verifies by
reading config back and probing the `CommitContext` query — you see the live
GAMM fee coin and fee-swap budget before calling it done. There is no
off-chain service to start before deploying; the distribution keeper is the
only standing job, and it only matters once a pool crosses. Set
`ADMIN_MULTISIG` to your multisig — it becomes both the wasmd migrate admin
and the in-contract admin, and mainnet deploys refuse to run without it.

---

## Links

- Website: https://www.bluechip.link/home
- Discord: https://discord.gg/gfdWgHFY
- Twitter: https://x.com/BlueChipCreate
