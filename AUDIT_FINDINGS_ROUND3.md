# Independent Adversarial Audit — Round 3 (router timelock + routed oracle)

**Scope of this pass:** ONLY the changes made after Round 2 —
- the **48h timelock on router registration** (`ProposeRouter` / `ApplyRouter`
  / `CancelRouter`, `PENDING_ROUTER`), and
- **routed (2-leg) pricing sources** (`PricingSource.usd_leg`, `UsdLeg`,
  `twap_pair_to_rate`, the routed branch of `probe_single_source`, leg
  validation).

**Posture:** a genuine fresh bug-hunt on this code, not just a feature write-up
— including re-deriving the composite price math against concrete on-chain
reserves and looking for a mispricing, a directionality flip, an overflow, or a
timelock bypass.

**Method:** re-read the changed files hunting for exploitable defects; found and
fixed a real **test-quality** gap; verified the math numerically; added
discriminating tests. Full suite after this pass: **creator-pool 159, factory
122, pool-core 8, router 24 — 0 failures** (21 oracle tests).

> Same honesty rule: not a "looks fine." Below is what I checked, the one thing
> I fixed, and what still needs human eyes.

---

## Testing-boundary caveat (unchanged)

Still unit tests with x/twap mocked. New this round: the mock serves per-pool
TWAP by decoding the request `pool_id`, so a routed source's **two** legs
resolve to two independent mock prices — the composite is exercised end to end
at the probe level (the on-chain price semantics themselves still need the
osmosis-test-tube harness).

---

## Confirmed bugs

**None.** The composite price math is correct (verified against concrete
reserves below), directionality matches the existing single-pool convention,
overflow is fail-closed, and the timelock cannot be bypassed. The one real issue
found was in the **tests**, not the contract (R3-1).

---

## Fixed during this review

### R3-1 — Routed-source tests were non-discriminating (test gap) — **FIXED**

- **What I found:** every routed-source test I first wrote produced a composite
  of exactly **$1.00** (2.0×0.5, 0.25×4.0, 5.0×0.2). A bug that simply returned a
  constant `1_000_000` — or dropped the second leg entirely — would have passed
  all of them. The plumbing was tested; the *arithmetic* was not.
- **Fix:** added tests that pin **non-unit** results and the code paths a
  constant-return bug can't fake:
  - `routed_pair_produces_correct_non_unit_rates` — 3.0×0.5=$1.50, 0.5×0.5=$0.25,
    8.0×0.25=$2.00, and a realistic 0.0005×1000=$0.50.
  - `routed_with_identity_leg_matches_direct` — a routed source with `D2 == 1.0`
    must equal the direct single-pool rate (cross-checks the two code paths).
  - `routed_integration_carries_non_unit_price` — a $2.00 routed source flows
    through the median as $2.00.
  - `manipulated_routed_source_is_deviation_filtered` — an $8 routed outlier is
    dropped by the deviation filter (proving it operates on the composite, not
    the raw legs).
  - `absurd_routed_composite_is_discredited` — huge legs fail the sanity ceiling
    and are discredited, not mispriced or panicked.

---

## Checked and found clean (with the derivation / test)

- **Composite math — verified against concrete reserves.** For an OSMO/BTC pool
  (OSMO 6-dec, BTC 8-dec) at OSMO=$0.50, BTC=$100k: leg-1 raw spot
  `D1 = ubtc/uosmo = 0.0005`; a BTC/USDC pool gives `D2 = uusdc/ubtc = 1000`;
  `rate = D1·D2·10^(12−6) = 0.5·1e6 = 500_000` = **$0.50**, correct. The
  intermediate token's decimals **cancel structurally** — `twap_pair_to_rate`
  does not even take an intermediate-decimals argument, so it *cannot* get BTC's
  8 / ATOM's 6 / AKT's 6 wrong. Only the USD stable's decimals are an input.
- **Directionality.** Leg 1 queries `(base=native, quote=source.quote_denom)`;
  leg 2 queries `(base=source.quote_denom, quote=leg.usd_denom)`; the product is
  `usd_raw/native_raw` — the same "quote-per-base raw ratio" convention the
  working single-pool path uses. No flip.
- **Overflow / precision.** The composite multiplies the two 18-dec atomics in
  `Uint256` with `checked_mul` (fail-closed → discredit, never panic), divides
  by `10^(24+usd_decimals)` with the exponent bounded (`usd_decimals ≤ 30`), and
  `Uint128::try_from` fails closed. Multiply-before-divide preserves full
  precision; realistic legs are nowhere near the overflow edge.
- **Deviation filter on composites.** Confirmed the filter discredits a
  manipulated *routed* source by its final composite value, so a two-pool source
  gets the same minority-outlier protection as a direct one.
- **Router timelock cannot be bypassed.** `ProposeRouter` (admin-only, rejects a
  second pending), `ApplyRouter` (admin-only, `env.block.time <
  effective_after` → `TimelockNotExpired`), `CancelRouter` (admin-only). The
  pool reads only the **applied** `ROUTER_ADDRESS`, so a pending proposal has no
  effect until the window elapses. Test:
  `router_registration_is_admin_only_and_timelocked`.
- **No liveness gap on rotation.** Proposing a new router leaves the *current*
  one active through the 48h window (routing keeps working); the first-ever
  router's pending window leaves `ROUTER_ADDRESS` unset, so pools fail **safe**
  (reject null-belief swaps) until it applies.
- **Backward compatibility.** `usd_leg` is `#[serde(default)] Option`, so a
  direct source (and any pre-existing config) is unchanged; the primary pool is
  hardcoded direct (`usd_leg: None`) and a routed primary is not supported.
- **Leg validation.** Shape is validated at propose/instantiate (leg `pool_id`
  non-zero, `usd_denom` non-empty and distinct from both the intermediate and
  `bluechip_denom`, decimals bounded). Test: `instantiate_rejects_malformed_usd_leg`.

---

## Findings carried forward (unchanged by this review)

The routed-source **security posture** from Round 2 still stands and is the main
thing to weigh — restated briefly:
- a routed source is a **weaker vote** (~2× manipulation/staleness surface: two
  pools must be honest and fresh);
- everything bottoms out on **USDC** (shared reliance);
- on-chain validation checks leg **shape**, not that the leg pool actually trades
  the declared pair (see below);
- prefer keeping the direct **USDC/OSMO** anchor and an **odd** source count.

Set `min_valid_sources` and `max_deviation_bps` accordingly (e.g. 3-of-4, ±5%).

---

## What I could not determine

1. **Leg-pair correctness on-chain.** The contract cannot verify that a leg
   `pool_id` genuinely trades `(quote_denom, usd_denom)` without querying pool
   assets; a leg pointed at a valid-but-wrong pool would return a real-looking
   price the deviation filter catches only if it diverges enough. **Verify every
   pool id off-chain before proposing.**
2. **Live per-denom x/twap semantics** for your specific mainnet pools/denoms —
   confirm against a live query before launch (Round 1/2 caveat).
3. Whether your chosen pools satisfy the depth/independence assumptions — depends
   on the ids you supply.
4. Real crossing/swap execution still lives behind the osmosis-test-tube harness.

---

## Test additions this round

| Test | Guards |
|---|---|
| `routed_pair_produces_correct_non_unit_rates` | composite arithmetic (non-unit) |
| `routed_with_identity_leg_matches_direct` | routed vs direct consistency |
| `routed_integration_carries_non_unit_price` | non-$1 price flows through median |
| `manipulated_routed_source_is_deviation_filtered` | deviation filter on composites |
| `absurd_routed_composite_is_discredited` | fail-closed on absurd legs |

Full suite: **creator-pool 159, factory 122, pool-core 8, router 24 — 0 failures.**
