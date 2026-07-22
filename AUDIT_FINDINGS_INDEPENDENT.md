# Independent Adversarial Audit — bluechip-osmosis launchpad

**Scope:** `factory`, `creator-pool`, `router`, `packages/pool-core`, `packages/pool-factory-interfaces`.
**Posture:** independent review — the repository already carries a two-round
self-audit (`AUDIT_REPORT.md`); this pass verifies its load-bearing claims
from source rather than trusting them, and hunts for what it missed.
**Method:** full read of all three contracts and the 458-test suite, adversarial
review of the areas below, then discriminating regression tests
(`creator-pool/src/testing/audit_tests.rs`, 7 new; whole suite 158 pass).

> **What I am NOT telling you:** that the contract is secure. Below is what I
> checked, what I found, and — importantly — what I could not determine at the
> unit level and needs human eyes or the `osmosis-test-tube` E2E harness.

---

## Testing-boundary caveat (read this first)

The creator-pool suite (mine included) is **unit tests with the Osmosis
modules mocked** — gamm, poolmanager, tokenfactory and x/twap do not execute;
a mock querier answers their reads, and **MockStorage does not roll back on
`Err`.** Consequences for what can and cannot be proven here:

- The **atomicity of the threshold crossing** (mints + `MsgCreateBalancerPool`
  committing or reverting together) cannot be shown by a unit test — it depends
  on real VM revert semantics. I reason about it from the code (see CLEAN-1)
  and it is exercised end-to-end only by `integration-tests/` (osmosis-test-tube).
- The **oracle/TWAP** is never computed in-crate; every test hard-codes the
  factory's rate response. No unit test constrains TWAP behaviour (see F-4).
- Real **GAMM swap execution** and **pool creation** don't run; tests assert on
  the *messages* the contract emits.

Where a finding lives on the far side of that boundary, I say so explicitly.

---

## Confirmed bugs

**None found.** I did not find a confirmed, exploitable fund-loss or
over-mint bug in the reviewed logic. The crossing/seed conservation, the
no-double-mint gate, the distribution pro-rata math, the router baseline math,
and the access-control gating are all correct as written, and the two prior
remediation rounds closed the material issues. The items below are real, but
they are design/trust concerns and uncertainties, not confirmed exploits — do
not read "no confirmed bugs" as "safe."

---

## Findings requiring a decision (design / trust / uncertain)

### F-1 — `SimpleSwap` is sandwichable when called directly without `belief_price` (Medium; decision needed)

- **Where:** `creator-pool/src/contract.rs:286-315` (SimpleSwap dispatch, no
  belief-price gate); protection derived in `packages/pool-core/src/swap.rs:98-149`
  (`derive_token_out_min`) and `:161-181` (`estimate_swap_out`).
- **What:** The post-threshold **commit** swap requires an explicit
  `belief_price` (H-3 fix, `creator-pool/src/commit/post_threshold.rs:73-75`).
  The public **`SimpleSwap`** entry point does **not** — with `belief_price:
  None` it dispatches `MsgSwapExactAmountIn` protected only by the on-chain
  *estimate floor*. The code itself documents (`swap.rs:75-88`) that the
  estimate floor is **not** sandwich protection: `estimated_out` is queried at
  *current* pool state, i.e. after any same-block front-run has already moved
  the price, so the floor only bounds the swap's own impact relative to the
  already-manipulated state.
- **Exploit path:** attacker front-runs a victim's direct `SimpleSwap` (no
  belief_price), moving the pool; the victim's estimate floor is computed on
  the moved price and passes; victim fills at the inflated price; attacker
  back-runs. Unbounded slippage bounded only by the 0.5% default relative to
  the manipulated mid.
- **Why it's a decision, not a bug:** the team's stated model
  (`AUDIT_REPORT.md`) is that `SimpleSwap` accepts null `belief_price` *for the
  router* (which enforces an end-to-end `minimum_receive`) and that the
  reference frontend always supplies `belief_price` for direct swaps. That
  makes on-chain safety depend on **off-chain frontend discipline**: a
  third-party integrator, a buggy UI, or a user calling the contract directly
  is exposed. You should decide whether direct `SimpleSwap` is a supported user
  entry point; if so, consider requiring `belief_price` on it too (as the
  commit path does) and letting the router pass a wide-but-present bound.
- **Test:** `simple_swap_accepts_null_belief_price_while_commit_requires_it`
  pins the asymmetry (SimpleSwap succeeds with null; commit rejects) so any
  future change is deliberate.

### F-2 — No pre-threshold exit; a crossing that can never succeed permanently locks committer funds (High impact / low-to-uncertain likelihood; decision needed)

- **Where:** `creator-pool/src/admin.rs:81-83` (emergency withdraw disabled
  pre-threshold, no cancel/refund handler exists); crossing revert surface in
  `creator-pool/src/commit/threshold_payout.rs:328-356` and `:441-449`.
- **What:** Pre-threshold pools have **no withdrawal or refund path** — this is
  acknowledged in the prior audit as "intended economic model" (H-02,
  dismissed). I re-surface it because it is the single largest fund-safety
  exposure and the board should decide with eyes open: any pool that never
  reaches its threshold locks every committer's OSMO forever.
- **Sharper sub-case (not fully covered by the prior audit):** the crossing
  reads the chain's **live** pool-creation fee (`query_pool_creation_fee_coin`,
  `packages/pool-core/src/osmosis_msgs.rs:158`). If Osmosis governance sets that
  fee to a denom that is **neither the native denom nor the configured
  `usd_quote_denom`**, `trigger_threshold_payout` returns `InvalidThresholdParams`
  (`threshold_payout.rs:345-355`) and **every** crossing attempt reverts — so a
  pool sitting mid-funding is bricked pre-threshold with committer funds locked.
  Factory config validation (`factory/src/execute/config.rs:155-164`) only
  constrains the *factory-configured* fee denom, not the *live* chain fee denom
  read at crossing. Likelihood is low (requires an unusual gov change to a
  third denom), impact is total, recovery is none.
- **Uncertain:** I cannot rule out other governance-parameter changes
  (e.g. fee amount ≥ raise, `threshold_payout.rs:441`) producing the same
  permanent-brick shape between a pool's creation and its crossing. This is a
  live-parameter dependency that needs an operational monitoring answer, not
  just a code answer.
- **Test:** the unroutable-fee-denom revert is already covered by
  `threshold_tests.rs::cross_denom_fee_unroutable_denom_errors` (the crossing
  errors rather than mis-charging) — the *consequence* (stuck funds) is the
  design gap, not a code defect.

### F-3 — The pool trusts the factory oracle rate with no sanity/freshness check of its own (Informational; trust boundary)

- **Where:** `creator-pool/src/commit.rs:227-229` — the only oracle guard at the
  pool is `if usd_rate.is_zero() || commit_value.is_zero()`. All real gating
  (RATE_MAX ceiling, sub-dust rejection, zero-TWAP rejection) lives in the
  factory (`factory/src/usd_price.rs:97-128`).
- **What:** `CommitContextResponse` carries a `timestamp` the pool ignores; the
  pool has no independent bound on a wrong-but-non-zero rate. In today's design
  the factory computes TWAP-to-now at query time (no caching), so staleness
  isn't realizable and the RATE_MAX ceiling does bound the value — so this is
  not currently exploitable. But the pool's correctness is **fully delegated**
  to the factory's valuation; a factory bug or compromise flows straight into
  every pool's threshold/distribution math with no pool-side backstop. Worth a
  defensive sanity bound at the pool boundary (e.g. reject absurd rates,
  cross-check the response timestamp).

### F-4 — TWAP manipulation of the pricing pool skews crossing valuation and distribution shares (Economic; needs mainnet-specific judgment)

- **Where:** valuation via `factory/src/usd_price.rs:49-92` (arithmetic TWAP,
  window 300–3600s configurable, `:44-45`); consumed for the threshold and for
  each committer's pro-rata weight (`creator-pool/src/commit/distribution_batch.rs:318-334`).
- **What:** both the crossing valuation and the 500B commit-return airdrop
  weights use the OSMO/USDC arithmetic TWAP. An attacker who can move the
  pricing pool for the full window can (a) inflate their own commit's USD credit
  → capture a disproportionate share of the airdrop *from honest committers*,
  and (b) shift how much real OSMO seeds the fresh xyk pool. Cost is bounded by
  pool depth × window and capped by the `RATE_MAX` $10k/native ceiling
  (`usd_price.rs:36`).
- **Uncertain — flagged for human eyes:** on Osmosis's deep OSMO/USDC pool with
  a ≥300s window this is expensive and likely uneconomic; on a **thin or
  misconfigured `pricing_pool_id`** (an admin-set, 48h-timelocked value) it is
  cheap. I cannot resolve exploitability without the specific mainnet pool depth
  and the chosen window. Decision inputs: which pool id is used, its depth, and
  the window length. This is the classic oracle-manipulation surface; the design
  mitigations (TWAP + window + RATE_MAX) are present and reasonable, but the
  residual is real and parameter-dependent.

### F-5 (minor, from router sub-review) — no explicit reentrancy guard on the router

- **Where:** `router/src/execution.rs` (whole module).
- **What:** The router holds no funds between transactions and its per-hop
  `offer_baseline` math makes an in-flight route's funds unreachable by a
  reentrant call — so there is no confirmed exploit. But the safety rests
  **entirely** on the baseline arithmetic being correct; there is no
  "route in progress" lock as defense-in-depth. If that arithmetic ever
  regressed, reentrancy would amplify it into a drain. Consider a cheap guard.
  Also a stale module doc (`execution.rs:26-34`) still describes the *pre*-fix
  "donations get swept" threat model and now contradicts the implemented
  baseline protection — correct it to avoid misleading future maintainers.

---

## Checked and found clean (with the test that would catch a regression)

- **CLEAN-1 — Crossing atomicity / no stranded or double-claimable funds.**
  `MsgCreateBalancerPool` rides as `SubMsg::reply_on_success`
  (`threshold_payout.rs:468-476`); on failure the reply is bypassed and the
  whole tx reverts — mints, ledger writes, `IS_THRESHOLD_HIT`, the excess
  refund, and fee sends all roll back, and the bank module returns the crosser's
  attached OSMO. So a failed crossing leaves the pool cleanly pre-threshold with
  nothing stranded and nothing double-claimable. The balance invariant
  `pool_osmo == net_raised + reserved` holds through refund/seed/remit
  (verified by hand and by the existing conservation proptest
  `invariant_tests.rs`). *Unit tests cannot prove the revert itself (MockStorage
  doesn't roll back) — this is the one place that genuinely needs the
  osmosis-test-tube E2E harness, which `integration-tests/` provides.*

- **CLEAN-2 — No double mint / no re-cross.** `IS_THRESHOLD_HIT` gate enforced
  at three sites (`threshold_crossing.rs:61`, `:196`; `threshold_payout.rs:182`).
  Covered by existing `*_rejects_when_flag_already_true` tests.

- **CLEAN-3 — Distribution: sum of allocations ≤ supply, no over-claim, dust to
  creator, last claimant can't be starved.** `sum(COMMIT_LEDGER) ==
  commit_amount_for_threshold_usd` exactly (crosser is ledgered for
  `value_to_threshold`, not full commit_value); each reward is a Uint256 floor
  (`distribution_batch.rs:318-334`); the residual mints to the creator on the
  final batch (`:191-229`); failed mints reconcile through `FAILED_MINTS` so the
  grand total is exactly `total_to_distribute`. **New tests:**
  `distribution_conserves_supply_across_whale_and_dust_committers` (whale+dust,
  multi-batch, conservation + no over-claim + creator dust) and
  `overshoot_crossing_keeps_ledger_sum_equal_to_threshold` (guards the
  ledger-sum invariant the pro-rata relies on).

- **CLEAN-4 — Overshoot refund is exact.** Prior suite only checked non-zero;
  new test `overshoot_crossing_refunds_exact_post_fee_excess_to_crosser` pins
  the exact post-fee excess coin (3_760_000 for the $5-crosses-$1 case).

- **CLEAN-5 — Crossing message ordering.** Seed/split mints and the refund
  (plain reply_never messages) all precede `MsgCreateBalancerPool`, and the
  `reply_on_error` factory-notify is dispatched after it — so the pool holds its
  seed when the create runs, and a factory-notify failure can't revert the
  crossing. New test
  `crossing_dispatches_seed_mints_before_pool_creation_and_notify_last`.

- **CLEAN-6 — Create-pool reply records POOL_ID (was untested).** The reply
  decodes `MsgCreateBalancerPoolResponse` and persists `POOL_ID`, which is what
  makes the pool swappable post-crossing; it fails loudly if the response is
  absent. New tests `create_pool_reply_records_pool_id_from_response` and
  `create_pool_reply_without_response_errors_rather_than_leaving_pool_id_unset`.

- **CLEAN-7 — Access control / privileged entry points.** Every pool admin op
  is gated on `info.sender == POOL_INFO.factory_addr`; factory config/pool/upgrade
  mutations are admin-only and 48h-timelocked, including a re-validation at
  apply time (`factory/src/execute/config.rs`, `.../upgrades.rs`);
  `NotifyThresholdCrossed` is bound to the *registered* pool address with an
  idempotency gate (`factory/src/execute/pool_lifecycle/admin.rs:155-201`) so a
  hostile contract can't forge a crossing. Covered by existing
  `test_unauthorized_*` / `test_factory_impersonation_prevented`.

- **CLEAN-8 — Migrate cannot skip the timelock.** Pool wasm admin is the factory
  (`create.rs:346`); the only path that emits `WasmMsg::Migrate` to a pool is the
  factory's `UpgradePools` flow, which checks `effective_after` on both
  `apply` and `continue` (`upgrades.rs:220-224`, `:326-343`). No propose→continue
  shortcut exists. Residual trust: whoever holds the *factory's* wasm-admin key
  could migrate the factory itself — standard governance assumption.

- **CLEAN-9 — Factory cannot be induced to create a hostile pool.** Pool params
  (threshold, fees, max-lock, payout splits) all come from **factory config**,
  not the creator's message; the creator supplies only token name/symbol/decimals
  (validated, `create.rs:94-148`) and a pair shape that is strictly validated
  (bluechip at index 0 with the canonical denom, creator placeholder at index 1).
  Covered by existing `instantiate_rejects_*` tests.

- **CLEAN-10 — Reentrancy / cw20 hooks.** The commit and swap paths run under
  `with_reentrancy_guard`. The creator token is a **native TokenFactory denom**,
  not a cw20 — there is no `Receive`/send-hook callback surface in the pool, and
  bank sends do not invoke contract code. The post-threshold swap forwards the
  **actual** `token_out_amount` decoded from the reply (`swap.rs:579-622`), not a
  balance query, so it can't be inflated by a mid-tx balance change.

- **CLEAN-11 — Router path validation and baseline sweep.** Every hop is checked
  against the factory registry **and** rejected if pre-threshold
  (`router/src/execution.rs:395-446`); the M-03 `offer_baseline` snapshot is
  correct on hop 0 and later hops (incl. the repeated-first-denom case), so
  donated/stray balances are never swept; `minimum_receive` is fail-closed and
  enforced end-to-end. (From a dedicated router sub-review.)

---

## What I could not determine

1. **True crossing atomicity under real VM revert** — provable only in the
   osmosis-test-tube E2E harness, not in the unit suite. The code is correct by
   construction (reply_on_success); the E2E is the confirmation.
2. **F-4 economic exploitability** — depends on the live `pricing_pool_id`
   depth and window; cannot be resolved without those mainnet parameters.
3. **F-2 full brick surface** — I confirmed the unroutable-fee-denom revert; I
   did not exhaustively enumerate every governance-parameter change that could
   make a crossing permanently revert. Needs an operational answer (monitoring
   + a pre-threshold exit path).
4. **The cross-denom fee-swap accounting on mainnet** (`threshold_payout.rs:328-356`)
   is internally consistent by my hand-derivation, but the exact-out swap
   leftover handling and the USDC-fee charge only truly exercise in E2E; the
   crate's `cross_denom_fee_tests` cover the message construction, not chain
   execution.

---

## Test additions

`creator-pool/src/testing/audit_tests.rs` — 7 tests, all passing; full suite
158 pass, 0 fail. Each asserts specific post-conditions (exact balances /
state / error variants), not merely that a call returned Ok:

| Test | Defends |
|---|---|
| `distribution_conserves_supply_across_whale_and_dust_committers` | pro-rata floor, no over-claim, creator dust, sum==supply (whale+dust) |
| `overshoot_crossing_keeps_ledger_sum_equal_to_threshold` | crosser ledgered for `value_to_threshold` only → distribution can't over-allocate |
| `overshoot_crossing_refunds_exact_post_fee_excess_to_crosser` | exact refund coin amount |
| `crossing_dispatches_seed_mints_before_pool_creation_and_notify_last` | message ordering: seed mint before create, notify after |
| `create_pool_reply_records_pool_id_from_response` | create-pool reply → POOL_ID set (pool becomes swappable) |
| `create_pool_reply_without_response_errors_rather_than_leaving_pool_id_unset` | reply fails closed on missing response |
| `simple_swap_accepts_null_belief_price_while_commit_requires_it` | pins the F-1 sandwich asymmetry |
