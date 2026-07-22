# Independent Adversarial Audit — Round 2 (the adjustments)

**Scope of this pass:** ONLY the changes made after Round 1 —
- **F-1** SimpleSwap `belief_price` gate + factory `SetRouter` / `RegisteredRouter`,
- **F-3** pool-side oracle ceiling,
- **F-5** router reentrancy guard,
- the **multi-pool median oracle** (`factory/src/usd_price.rs`, config, mock).

**Posture:** the same tier-1 adversarial review applied to Round 1, now turned
on the code that was just written — reviewing my own diffs with fresh, hostile
eyes rather than trusting them. Findings trace to specific lines; confirmed
issues are separated from honest limitations from checked-and-clean.

**Method:** re-read every changed file, reasoned about new attack surface and
new coupling, fixed one real issue found in the process, and added
discriminating tests. Full suite after this pass: **creator-pool 159, factory
115, pool-core 8, router 24 — 0 failures.**

> As before: I am not telling you it's secure. Below is what the adjustments
> did, what I found in them, and what still needs human eyes / the E2E harness.

---

## Testing-boundary caveat (unchanged, still load-bearing)

These remain **unit tests with the Osmosis modules mocked**. New for this pass:
the median oracle's decimal normalization is validated against the *documented*
x/twap semantics (`arithmetic_twap` returns `quote_raw / base_raw`), NOT against
a live pool. The one exception is F-5, where the "no-wedge" property genuinely
needs revert semantics and is now covered by a **cw-multi-test** case (real
rollback), not a mock.

---

## Confirmed bugs

**None outstanding.** One real issue was found *and fixed* during this pass
(R2-1 below); nothing else rose to a confirmed exploit. The remaining items are
honest limitations / new couplings the adjustments introduced, which you should
weigh before launch.

---

## Fixed during this review

### R2-1 — Median oracle did not deduplicate pricing sources (Medium) — **FIXED**

- **Where:** `factory/src/execute/config.rs` (oracle validation).
- **What I found:** nothing stopped an admin from listing the same `pool_id`
  twice — either an extra source reusing `pricing_pool_id`, or two extras with
  the same id. Each occurrence became an independent vote in the median, so a
  single manipulated pool would get **multiple correlated votes** and drag the
  median toward itself — defeating the entire point of a multi-pool oracle
  (one independent vote per pool).
- **Fix:** validation now rejects a duplicate `pool_id` anywhere in the source
  set (primary + extras) at propose/instantiate time, with a clear error. A
  multi-asset pool that could price against several denoms must be listed once.
- **Test:** `factory/src/testing/oracle_tests.rs::instantiate_rejects_duplicate_pool_id`
  (both the primary-collision and extra-collision cases).

---

## Findings requiring a decision (honest limitations / new couplings)

### R2-A — "Require belief_price" forces intent but does NOT guarantee a tight bound (Medium; inherent)

- **Where:** `packages/pool-core/src/swap.rs:98-149` (`derive_token_out_min`);
  gate at `creator-pool/src/contract.rs` (SimpleSwap) and
  `creator-pool/src/commit/post_threshold.rs:73-75` (commit).
- **What:** the F-1 fix (and the earlier H-3 commit fix) reject a *null*
  `belief_price`, but a caller may still pass a **loose/meaningless** one. A
  huge `belief_price` makes the belief floor `≈ offer / belief_price ≈ 0`, so
  `token_out_min = max(estimate_floor, ~0) = estimate_floor` — the estimate
  floor is queried at already-front-run state and is **not** sandwich-resistant.
  So a direct caller who supplies a bad bound is still sandwichable.
- **Net effect of the fix:** it eliminates the *accidental-omission* footgun
  (null now hard-fails, forcing the frontend to choose a value) and makes the
  router — bounded by end-to-end `minimum_receive` — the only null-belief path.
  It does **not** make direct swaps unconditionally safe; that still depends on
  the frontend supplying a *meaningful* belief price from a live quote.
- **Decision:** accept the frontend dependency (matches the commit path), or add
  a coarse server-side sanity on the belief price. A truly tight on-chain bound
  is not achievable without an independent reference price at swap time.

### R2-B — Null-belief SimpleSwap now has a liveness dependency on the factory (Low; consequence of the chosen design)

- **Where:** `creator-pool/src/contract.rs` SimpleSwap arm →
  `creator-pool/src/swap_helper.rs::query_registered_router`.
- **What:** to resolve the router exemption, the pool now **queries the factory
  on every null-belief SimpleSwap** (one query per router hop). Before F-1,
  `SimpleSwap` had no factory dependency at all. If the factory is unreachable
  or mid-migration, every null-belief swap — i.e. **all router routes** — fails
  closed. Direct swaps that carry a `belief_price` skip the query and are
  unaffected. This is the tradeoff of the "exempt-by-address-via-query" approach
  you chose over storing the router address on the pool at instantiate.
- **Decision:** acceptable if the factory is treated as always-available critical
  infra (the commit path already depends on it). If you want to decouple router
  swaps from factory availability, store the router address on the pool instead
  (more instantiate/config plumbing, no per-swap query).

### R2-C — router registration is now 48h-timelocked — **FIXED**

- **Where:** `factory/src/execute.rs` (`execute_propose_router` /
  `execute_apply_router` / `execute_cancel_router`), `PENDING_ROUTER` state.
- **What changed:** the direct `SetRouter` admin op was replaced with the
  standard `ProposeRouter → wait 48h → ApplyRouter` (plus `CancelRouter`) flow,
  mirroring the factory-config timelock. A change to who is exempt from the
  SimpleSwap belief gate is now observable for the full window, and a
  compromised admin key cannot repoint it instantly. `RegisteredRouter` reflects
  only the APPLIED value.
- **Test:** `factory/src/testing/coverage_gap_tests.rs::router_registration_is_admin_only_and_timelocked`
  (non-admin rejected; proposed-but-unapplied has no effect; apply-before-window
  rejected; apply-after-window takes effect).

### R2-D — The median oracle assumes an honest MAJORITY of configured pools (Info; inherent to median oracles)

- **Where:** `factory/src/usd_price.rs::probe_median_usd_rate`.
- **What:** median + deviation filter absorbs a *minority* of manipulated or
  outlier pools. If an attacker manipulates a **majority** of the configured
  sources for the full TWAP window, they set the median AND the deviation filter
  will discredit the honest minority as the outliers. This is the standard
  oracle trust assumption, not a defect — but it means the security of the whole
  valuation reduces to "enough independent, deep pools that no attacker can move
  a majority for the window." Choose sources accordingly.

### R2-E — No per-pool liquidity-depth check (Info; possible enhancement)

- **What:** a source is validated by TWAP-query success + price sanity +
  cross-source deviation. It is **not** checked for liquidity depth. A thin pool
  that passes the query and happens to agree with consensus is trusted; it only
  gets discredited once it *diverges* past the deviation band. A cheap-to-move
  thin pool is therefore a latent weak vote. Mitigation today: only configure
  deep pools. Enhancement: add an optional per-source minimum-liquidity gate
  (one extra query per source).

### R2-F — The PRIMARY pool's quote denom is assumed 6-decimal (Info; constraint)

- **Where:** `factory/src/usd_price.rs::pricing_sources` pins the primary source
  to `quote_decimals: 6`.
- **What:** extra sources carry an explicit `quote_decimals`, but the primary
  `usd_quote_denom` does not — it is hardcoded 6. A deployment whose primary
  quote denom is not 6-decimal would be mispriced (and most likely discredited
  by the ceiling, bricking valuation). This inherits the pre-existing
  single-pool assumption. Document the constraint, or add a `usd_quote_decimals`
  field for the primary if you ever pair against a non-6-decimal stable.

### R2-G — Even-count median averages the two middle values (Info; minor)

- **What:** with an even number of surviving sources, the median is the
  floor-average of the two middle rates, so an attacker controlling one of the
  two middle pools shifts the result by ~half its deviation (still bounded by
  the deviation filter + quorum). Prefer an **odd** number of independent
  sources so the median is a single real observation.

---

## Checked and found clean (with the test that guards a regression)

- **F-1 gate logic.** Null belief → `BeliefPriceRequired` unless
  `sender == registered_router`; an unset router ⇒ every null-belief swap is
  refused (fail-safe); a factory query error ⇒ swap fails closed. A caller who
  *does* pass a belief price skips the query.
  Test: `direct_simple_swap_requires_belief_price_but_registered_router_is_exempt`.
- **F-1 `SetRouter` / `RegisteredRouter`.** Admin-only, address-validated; the
  query reflects stored state (None → address). A rejected set does not mutate.
  Test: `set_router_is_admin_only_and_query_reflects_it`.
- **F-3 ceiling.** A rate above `POOL_RATE_MAX` is rejected with no ledger
  write; a normal rate passes. The bound is intentionally a **ceiling only** —
  a low rate makes crossing *harder*, not a theft vector, and
  `usd_to_native_at_rate` is checked-math so a low rate can't overflow the
  crossing. Test: `commit_rejects_oracle_rate_above_pool_ceiling`.
- **F-5 guard — no wedge.** A nested `ExecuteMultiHop` is rejected while a route
  is in progress; and — the important one — a **failed** route rolls the guard
  back (revert), so the next route still works. The guard is set before all
  validation, so any failure after that point reverts it; success clears it in
  the terminal `AssertReceived`. Tests: `nested_multi_hop_is_rejected_while_route_in_progress`
  (unit) and `failed_route_does_not_wedge_the_reentrancy_guard` (cw-multi-test,
  real rollback).
- **Median oracle math.** Median (odd → middle, even → floor-average);
  mixed-decimal normalization (6-dec and 18-dec $1 both → `1_000_000`);
  dead-pool discredit; sanity-ceiling discredit; quorum fail-closed with
  per-source reasons; deviation discredit of a manipulated pool; single-source
  legacy parity; duplicate rejection. Tests: `oracle_tests.rs` (11).
- **Oracle overflow safety.** Normalization runs in `Uint256` with `checked_mul`
  and a fail-closed `Uint128::try_from`; the power-of-ten exponent is bounded
  (`quote_decimals ≤ 30`); `probe_single_source` has no panic path, so a
  hostile/huge TWAP string discredits that source rather than aborting the
  probe. The deviation arithmetic (`diff * 10_000 ≤ med * max_bps`) stays within
  `Uint256` even for a `u64::MAX` deviation config.
- **Backward compatibility.** Empty `extra_sources` + default thresholds ⇒
  byte-identical legacy single-pool behavior; `#[serde(default)]` on the new
  `oracle` field lets old serialized factory records deserialize.
  Test: `single_primary_source_matches_legacy_behavior`.
- **Cross-denom fee swap unaffected.** The median is factory-side; pools still
  receive a single `rate_used`, and the GAMM-fee swap still routes through the
  single primary `pricing_pool_id` — no new coupling on the crossing path.

---

## What I could not determine

1. **Live x/twap decimal semantics per denom.** The normalization is correct
   against the documented `quote_raw/base_raw` model; confirm the actual price
   scale for each *specific* mainnet pool/denom you configure against a live
   query before launch (especially any non-6-decimal quote).
2. **Whether your chosen pools satisfy the honest-majority + depth assumptions**
   (R2-D / R2-E) — depends entirely on the pool ids you supply. Pick deep,
   independent pools; an odd count (R2-G).
3. **Real crossing/swap execution** still lives behind the osmosis-test-tube
   harness, unchanged from Round 1.

---

## Test additions this round

| Test | File | Guards |
|---|---|---|
| `instantiate_rejects_duplicate_pool_id` | factory `oracle_tests.rs` | R2-1: one vote per pool |
| `failed_route_does_not_wedge_the_reentrancy_guard` | router `integration_tests.rs` | F-5: guard rolls back on failure |
| (10 median-oracle tests) | factory `oracle_tests.rs` | median / normalization / discredit / quorum / deviation / legacy parity |

Full suite: **creator-pool 159, factory 115, pool-core 8, router 24 — 0 failures.**
