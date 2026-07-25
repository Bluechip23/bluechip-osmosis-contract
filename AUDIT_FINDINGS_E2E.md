# Real-Chain E2E Validation — bluechip-osmosis launchpad

> **⚠ SUPERSEDED (oracle sections only).** This report validated the **x/twap
> multi-pool median** oracle. That oracle was subsequently **replaced by Pyth**
> (the OSMO/USD pool substrate proved too thin to price safely — see this
> report's own MAINNET_LIQUIDITY_RECON findings). The `oracle.extra_sources` /
> median / routed-source content below is a historical record and **no longer
> describes the shipping code**. For the current oracle see
> `ORACLE_PYTH_TRANSITION.md`, `TESTNET_VALIDATION_PYTH.md`, and the RUNBOOK.
> Everything NON-oracle here (crossing atomicity, distribution, router, F-1/F-5,
> the E2E-1 cosmwasm-check fix) still stands.

**Scope:** run the protocol against a real Osmosis chain and resolve, from
execution rather than reasoning, the open items the three prior reports
(`AUDIT_FINDINGS_INDEPENDENT.md`, `_ROUND2.md`, `_ROUND3.md`) left in their
"What I could not determine" sections.

**Method:** `osmosis-test-tube` — the contracts run against a real in-process
Osmosis chain, so `gamm`, `poolmanager`, `tokenfactory`, and `x/twap` actually
execute (no mock querier, and — critically — **real VM revert semantics**,
which unit tests do not have). The tests were written to be *discriminating*:
each asserts a concrete post-condition (an exact rate, an exact balance, an
exact rolled-back state), chosen so that a plausible-but-wrong implementation
produces a different number, not merely a non-`Ok` result.

**Posture (unchanged from the prior rounds):** this does not tell you the
contract is secure. It tells you which previously-unprovable claims now hold
under real execution, what I had to fix to get there, and what still cannot be
closed without inputs I do not have.

> **The headline is not "all green."** Three things came out of this pass that
> you should act on:
>
> 1. A **build-blocking defect**: the optimized `factory.wasm` was rejected by
>    `cosmwasm-check` and **could not have been uploaded on-chain** (E2E-1).
>    Fixed; new artifact hashes below.
> 2. The **default oracle config is the exploitable one.** Measured on a real
>    chain: with an empty `extra_sources`, corrupting one pool for one TWAP
>    window bought an attacker **~4× the airdrop weight** for the same money
>    ($3,980 of commit credit vs an honest committer's $1,000). The same attack
>    against the 5-source median config yielded **exactly zero**. Do not ship
>    with `extra_sources` empty.
> 3. **There is no on-chain liquidity-depth protection at all.** A pool with
>    1/1000th its peers' depth is a full, equal vote, and draining a source by
>    99% is completely invisible to the contract while cutting its manipulation
>    cost ~100×. With 7 sources this is a *minor* weakness (one thin pool
>    discounts the attack ~25%), but it is entirely on you to police depth
>    off-chain — the contract will never tell you.

---

## What I ran

| Suite (real chain) | Tests | Result |
|---|---|---|
| `integration-tests/tests/lifecycle.rs` (pre-existing) | 4 | **4 pass** |
| `integration-tests/tests/oracle_e2e.rs` (**new**) | 2 | **2 pass** |
| `integration-tests/tests/oracle_economics_e2e.rs` (**new**) | 9 | **9 pass** |
| `integration-tests/tests/crossing_atomicity_e2e.rs` (**new**) | 2 | **2 pass** |
| `integration-tests/tests/router_e2e.rs` (**new**) | 1 | **1 pass** |
| **Total real-chain** | **18** | **18 pass** |
| Workspace unit suite (post-fix regression) | creator-pool 159 / factory 125 / pool-core 8 / router 24 | **0 fail** |

Build/verify gate: `make optimize-all` (cosmwasm/optimizer 0.16.0) → `make
check` (cosmwasm-check 3.0.9) — **now 3/3 after the fix below** (it was 1
failure before).

Phase 2 (real osmo-test-5 deploy) was **not run** — it is blocked on inputs I
do not have; see the last section. Nothing was pushed to git; mainnet files
were not touched.

---

## Confirmed issues found during this pass

### E2E-1 — `make check` was failing: `factory.wasm` rejected by cosmwasm-check (Build blocker) — **FIXED**

- **What:** the freshly optimized `artifacts/factory.wasm` fails static
  validation:
  `Error during static Wasm validation: Wasm contract contains function with
  more than 100 locals: 101`. cosmwasm-check ≥ 3.x (matching cosmwasm-vm)
  enforces a hard 100-locals-per-function ceiling **at StoreCode**, so this
  artifact would be **rejected on upload** to any current-version Osmosis
  chain. This is not cosmetic — it blocks deployment of the factory.
- **Cause:** the Round-1 multi-pool-oracle change added the
  `oracle: MultiOracleConfig` field to `FactoryInstantiate`. The serde-derived
  deserializer for that struct is one monolithic function; the extra field
  pushed it to 101 locals — one over the limit. The canonical migration bytes
  recorded in the project memory (`d0b9f18f…`, codes 13258-13260) predate this
  and share the defect: they cannot be re-stored on a current chain.
- **Fix:** box the field — `pub oracle: Box<MultiOracleConfig>`
  (`factory/src/state.rs`). Boxing moves the field's deserialize temporaries
  off the wasm-locals frame; `serde` and `schemars` treat `Box<T>`
  transparently, so **the JSON wire format and the stored state are
  byte-identical** — no migration, no client change. The offending function
  drops to **99 locals** and `make check` passes 3/3. The only other touch is
  the single literal constructor in `factory/src/testing/oracle_tests.rs`
  (`Box::new(...)`). The pinned `deploy_script_instantiate_json_deserializes`
  test still passes, confirming wire-compatibility.
- **New artifact hashes (post-fix, cosmwasm-check-clean):**
  - `factory.wasm` `b01f070294af111edb1545492f5cbb53990f672eb79388c7492fad8fcc7af64d`
  - `creator_pool.wasm` `d0778ea3e418b0ac325a7b546d3d048c4053a3714cc9de57956365563f404fad`
  - `router.wasm` `ea5c69bb716932c0b879780f904437c6606a4c38a25a1d1f0d2bf654207a64ac`

  **Re-store these before any deploy; do not reuse the pre-fix factory bytes.**

### E2E-2 — the harness's cross-denom-fee test was not testing what it claimed (Test defect) — **FIXED**

- **What:** `factory_config_with_gamm_fee` in `lifecycle.rs` took a
  `gamm_pool_creation_fee: Coin` parameter and then **hardcoded**
  `Coin::new(GAMM_CREATE_FEE, UOSMO)` in the struct, silently discarding the
  argument. The `cross_denom_usdc_fee_crossing_swaps_and_creates_pool` test
  passes a `20 uusdc` fee to model the osmosis-1 mainnet shape — but the
  factory was still configured with a **uosmo** fee, so the factory-side
  validation of a USDC-denominated configured fee was never exercised
  (the crossing still worked because the crossing reads the *live* chain fee,
  not the config). The test proved less than its name implied.
- **Fix:** pass the argument through. The USDC-fee crossing test now actually
  configures the factory with the USDC fee and still passes — so both the
  factory-config path and the live-fee path are now genuinely covered.

---

## Resolution of every "What I could not determine" item

Each item below is quoted (condensed) from the prior reports, then marked
**RESOLVED**, **PARTIALLY RESOLVED**, or **STILL OPEN**, with the test that
settles it.

### From `AUDIT_FINDINGS_INDEPENDENT.md`

**(1) "True crossing atomicity under real VM revert — provable only in the
osmosis-test-tube E2E harness."** → **RESOLVED (clean).**
`crossing_atomicity_e2e::mid_crossing_module_failure_reverts_mints_ledger_fees_and_funds`
forces a **real module-level failure in the middle of the crossing's message
chain** — after the 5%/1% fee bank-sends and all three TokenFactory `MsgMint`s
have already *executed* — by setting the live pool-creation fee to more USDC
than the pricing pool holds, so the crossing's `MsgSwapExactAmountOut` fee leg
fails inside `x/gamm`. The chain error confirms the failure point
(`RouteExactAmountOut failed … base must be greater than 0`). Post-revert the
test asserts, on real chain state, that **everything rolled back**: pool still
`InProgress` with `raised == 0`, `IS_THRESHOLD_HIT` unset, **token supply back
to 0** (the executed mints reverted — the exact thing MockStorage cannot show),
no native pool id recorded, nothing stranded in the pool, the creator's and
bluechip wallet's balances **exactly** unchanged (fee sends reverted), and the
crosser's 26,000 OSMO returned (only gas lost). Then it proves **no wedge**: the
same pool crosses cleanly once the fee is payable, and the `POOL_ID` is read
from the *real* `MsgCreateBalancerPoolResponse` protobuf and is swappable. This
is CLEAN-1 confirmed by execution, not by construction.

**(2) "F-4 economic exploitability — depends on the live pricing_pool_id depth
and window."** → **RESOLVED as a formula + a measured impact.** This did *not*
actually need the mainnet pool id: what depends on the pool is one number
(its depth), and the relationship between depth and attack cost is a property
of constant-product pools that reproduces exactly in the harness.
`oracle_economics_e2e.rs` establishes:

  - **The cost law (`manipulation_capital_scales_linearly_with_pool_depth`).**
    Three pools spanning **100× in depth** were each hit with the same
    *fraction* of their reserve (12%) and all three landed on **exactly the
    same** resulting price, $1.2531 — price impact is scale-invariant, so
    attack capital is strictly proportional to depth:

    > **Moving a pricing source +25% costs ≈ 12% of that pool's quote-side
    > reserve.** A $1,000,000-deep source ⇒ ~$120,000. A $50,000-deep source
    > ⇒ ~$6,000.

    You can now answer F-4 for any candidate pool by reading one number off
    an explorer, with no further testing.
  - **The measured impact
    (`manipulated_single_source_steals_commit_credit_median_prevents_it`).**
    The damage is a pro-rata airdrop share, and the airdrop is strictly
    proportional to each committer's USD commit credit — so the theft is
    directly measurable. With the **legacy single-source config**, an attacker
    who corrupted one pool for one TWAP window was credited **$3,980 for the
    same 1,000 OSMO that earned an honest committer $1,000** — a **~4×**
    airdrop weight multiplier, diluted straight out of honest committers, with
    their principal fully recoverable. With the **5-source median config** the
    identical attack produced **exactly equal credit ($1,000 vs $1,000) — zero
    effect.**

    > **Operational consequence:** the exploitable configuration is the
    > *default*. `MultiOracleConfig::default()` (empty `extra_sources`) is the
    > single-source legacy path. **Do not ship with an empty `extra_sources`.**
  - **There is NO price snapshot, and the TWAP window is the real defence
    (`momentary_spike_is_absorbed_the_attacker_must_hold_the_full_window`).**
    Worth stating explicitly because it is easy to assume otherwise: the
    factory persists **config only** — there is no stored rate, no cache, and
    no "update cycle". `probe_median_usd_rate` calls
    `arithmetic_twap_to_now(now − window, now)` **fresh on every single
    valuation**. So an attacker never waits for a refresh that arbitrageurs
    could beat; they can commit inside their own manipulation.

    What actually stops them is the **window**. Spiking the spot price ~4× and
    reading the oracle in the same breath moves it **not at all**, because the
    300s lookback is still full of honest observations. The oracle only
    converges as the attacker *holds* the pool off-market:

    | Held | Oracle sees |
    |---|---|
    | 0s | **$1.0000** (spot is already $3.98) |
    | 75s | $1.7426 |
    | 150s | $2.4852 |
    | 225s | $3.2278 |
    | 300s (full window) | **$3.9704** |

    A clean linear time-weighted ramp. The attacker must fund the position and
    defend it against arbitrage (and `x/protorev`) for the **entire** window to
    realise the full effect — and a partial hold yields only a proportional
    fraction. **`twap_window_seconds` is therefore a direct security dial:
    longer window ⇒ strictly costlier attack.** Deployed value is 600s (the
    configurable range is 300–3600s); raising it is the cheapest available
    hardening if you want more margin.
  - **New finding — `x/protorev` partially self-corrects manipulation.** While
    building these tests I found that a large manipulation swap on one Osmosis
    pool *also moved an unrelated pool*. The cause is Osmosis's in-protocol
    arbitrage module (`x/protorev`): it backruns the dislocation an attacker
    creates and routes the corrective trade through the OSMO/USDC anchor. This
    is a real, unmodeled, *defensive* factor — an attacker holding a pool
    off-market for a full TWAP window is fighting in-protocol arbitrage the
    whole time. The tests **disable** protorev so the attacker's job is
    strictly easier than reality, which means **every capital figure above is a
    lower bound** on real-world difficulty.

  What remains genuinely parameter-dependent is only the plug-in: your chosen
  pools' actual depths, and whether their *combined* corruption cost exceeds
  what an attacker gains (bounded by the 500B commit-return airdrop's value).

**(3) "F-2 full brick surface — did not exhaustively enumerate every governance
change that could permanently revert a crossing."** → **PARTIALLY RESOLVED.**
`crossing_atomicity_e2e::unroutable_live_fee_denom_bricks_crossing_until_restored`
exercises the sharpest sub-case on a real chain: governance re-denominates the
live pool-creation fee to a third denom (neither native nor the quote denom).
Every crossing attempt then reverts with the *actionable* error
(`… neither the native denom …`) and — the important part — **takes nothing**:
funds returned, ledger untouched, across repeated attempts. When governance
restores a payable fee, **the same pool crosses cleanly**. So the brick is
exactly as wide as the bad parameter and is **fully recoverable** — it is a
liveness pause, not permanent state damage or fund loss. What remains open is
the same design point the prior report raised: there is still **no
pre-threshold committer exit**, so committers' funds are *locked* (not lost)
for the duration of any such brick. That is a design decision for the owner
(monitoring + an exit path), not a code defect — unchanged by this pass.

**(4) "Cross-denom fee-swap accounting on mainnet … only truly exercises in
E2E."** → **RESOLVED (clean).** The pre-existing
`cross_denom_usdc_fee_crossing_swaps_and_creates_pool` (now genuinely
configuring a USDC fee after the E2E-2 fix) drives the full chain on real
modules — 1% retention → `MsgSwapExactAmountOut` (uosmo→USDC via the pricing
pool) → `MsgCreateBalancerPool` (module charges the USDC) — and asserts the
crossing succeeds, the native pool exists, and **no USDC dust is stranded**
(exact-out leaves zero). The atomicity test additionally proves the *failure*
direction of this exact path reverts cleanly.

### From `AUDIT_FINDINGS_ROUND2.md` and `_ROUND3.md`

**(R2/R3-1) "Live x/twap decimal semantics per denom / real crossing-swap
execution behind the harness / whether the composite price math is right on a
live chain."** → **RESOLVED (clean).** `oracle_e2e.rs` proves the oracle
against real `x/gamm` + `x/twap`, with **exact** expected values a
constant-return or dropped-leg bug could not fake:
  - **Median on live TWAPs.**
    `median_oracle_prices_real_commits_and_discredits_bad_pools` stands up six
    real pools (prices $0.98–$20,000, one quoted in a *different* stable
    `uusdt`, one younger than the TWAP window) and asserts the factory's live
    `ConvertNativeToUsd` returns **exactly the median of the credible ones
    ($1.01)** — the young pool discredited by the chain itself, the $20k pool by
    the live `RATE_MAX` ceiling. A primary-only bug reads $1.20; a
    mean-instead-of-median bug reads $1.05 — both excluded.
  - **A real commit is valued at the median.** A live `Commit` lands on the
    pool ledger at exactly `median × amount`.
  - **Manipulation → deviation discredit, on-chain.** A **real whale swap**
    moves one pool ~4×; after a full TWAP window the median moves to exactly
    $1.06 (manipulated pool deviation-dropped, the matured pool now credited),
    and a stricter-quorum factory **fails closed**, with a live commit against
    it **reverting and taking nothing**.
  - **Routed 2-leg composite, live.**
    `routed_two_leg_sources_price_native_in_usd_from_real_pools` prices the
    native asset through OSMO/BTC×BTC/USDC (**8-decimal** intermediate) and
    OSMO/ATOM×ATOM/USDC (**6-decimal** intermediate) and asserts both land on
    **exactly $0.50** — confirming on a real chain the claim that *the
    intermediate token's decimals cancel* and only the USD stable's decimals
    matter. A source with a dead second leg is discredited; a real single-leg
    crash leaves the median bounded (exactly $0.55, not the crashed value).
  - **Config gates, live.** `RATE_MAX` refuses an absurd primary at instantiate;
    a duplicate pool id (R2-1) is refused at instantiate — both against the real
    probe.

**(R2/R3-2) "Whether your chosen pools satisfy the honest-majority + depth
assumptions."** → **The ASSUMPTIONS are now measured; only the plug-in numbers
remain deployment-specific.** Two experiments settle the general behavior:

  - **R2-D — the honest-majority boundary is exactly ⌈n/2⌉
    (`honest_majority_boundary_at_three_of_five`).** Five live sources at
    $1.00, ±25% deviation filter; corrupted to ~$4 one at a time:

    | Corrupted | Oracle rate | Outcome |
    |---|---|---|
    | 1 of 5 | **$1.00** | attacker's pool deviation-dropped |
    | 2 of 5 | **$1.00** | still a minority; both dropped |
    | **3 of 5** | **$3.98** | **attacker owns the median** |

    At 3-of-5 the provisional median *is* the attacker's price, so the
    deviation filter **inverts and discredits the two HONEST pools as the
    outliers** — R2-D's warning confirmed by execution, not by argument. The
    security property reduces exactly to: *no attacker can move ⌈n/2⌉ sources
    for the full TWAP window.* Combined with the cost law above, that is now a
    dollar figure: **sum the 12%-of-depth cost of your cheapest ⌈n/2⌉ sources.**
  - **R2-E — there is NO depth gate, and liquidity decay is invisible
    (`thin_pool_is_a_full_vote_and_liquidity_decay_is_invisible`).** This is
    the direct answer to "will the contract reject a thin pool, and will it
    notice liquidity changing?" — **it will not, on both counts.** A pool with
    **1/1000th** the depth of its peers was accepted at instantiate (live
    probe) and counted as a **full, equal vote** toward a 3-of-3 quorum, purely
    because its price agreed. Then a real `MsgExitPool` drained the **primary**
    source by 99%; because a proportional exit removes both sides equally, the
    **price was unchanged** — and the factory kept pricing from it at exactly
    $1.00, with no error, no flag, and no reduction in its weight. The same
    capital that moved the intact peer by 0.24% moved the decayed source to
    **$1.2531 (+25%)**: its corruption cost fell ~100× and **nothing on-chain
    registered the change.**

    > **Consequence:** pool depth is an **off-chain operational control only**
    > (the RUNBOOK's liquidity-floor alarm). The contract will not catch decay
    > for you, and `x/twap` never reports "thin". If you want an on-chain
    > backstop, R2-E's suggested per-source minimum-liquidity gate is the
    > change — it does not exist today.

  - **How much does one thin pool actually matter with 7 sources?
    (`with_seven_sources_one_thin_pool_is_only_one_cheap_vote_of_the_four_needed`)**
    Not much — priced exactly. Six deep sources plus one 1000×-thinner one,
    corrupted cheapest-first:

    | Corrupted | Capital spent | Oracle rate |
    |---|---|---|
    | 1 of 7 (the thin one) | 0.01 B | **$1.00** — untouched |
    | 3 of 7 | 20.01 B | **$1.00** — median holds |
    | **4 of 7** = ⌈7/2⌉ | 30.01 B | **$3.98** — attacker owns it |

    Against 40.0 B if all seven were deep, so **the thin pool discounts the
    whole attack by only ~25%** — it is one nearly-free vote out of the four
    required. **One thin source among seven is a minor weakness; four thin
    sources would be fatal.** The correct way to price a source set is the
    cost of the **cheapest ⌈n/2⌉ pools, not the average** — the attacker
    chooses which to attack.

  - **⚠ THE INTENDED SET AS SPECIFIED IS 4 VOTES, NOT 7 — and it has a cheap
    liveness DoS
    (`owners_four_source_set_falls_to_weakest_legs_and_has_no_odd_majority`).**
    The set given in the task — 1 direct USDC/OSMO + 3 routed (BTC, ATOM, AKT)
    — is **seven pool ids but only FOUR pricing sources**, because
    `pricing_sources()` emits **one vote per `PricingSource`** and a routed
    source spends two pools to produce one vote. Tested on real pools, with
    the realistic shape where the OSMO/<asset> pairs are deep (100 B) but the
    <asset>/USDC legs are 100× thinner (1 B):

    | Corrupted | Capital | Result |
    |---|---|---|
    | 1 of 4 (via its weak leg) | 1 B | **$1.00** — deviation-dropped, median holds |
    | **2 of 4** (via weak legs) | **2 B** | **ALL FOUR discredited → quorum fails → every commit reverts** |

    Two distinct problems, both measured:
    1. **Even count has no majority to fall back on (R2-G is load-bearing
       here, not cosmetic).** With a 2/2 split the provisional median lands
       *between* the camps, so the ±25% filter discredits **every** source and
       the valuation fails closed. That is fail-*safe* — no mispricing, no
       theft — but it is a **cheap total liveness DoS**: no commit can be
       priced on any pool until the attacker stops.
    2. **A routed source costs `min(leg depths)` to corrupt, not the depth of
       its headline pair.** The attacker moved only the thin <asset>/USDC
       legs; the deep OSMO pairs were never touched. The whole attack cost
       2 B against 600 B of deployed liquidity it never had to move.

    **The rule that actually governs (tested both ways).** My first
    recommendation — "add one direct anchor to make it odd, 2 direct + 3
    routed" — was **not sufficient**, and the harness caught it:
    `recommended_five_source_set_at_800s_window_absorbs_the_two_source_attack`
    shows that set absorbs the 2-source attack that bricked the 4-source
    config, **but still falls at 3 of 5 for 3 × weak-leg capital** — because
    the three thin-legged routed sources are *themselves* a majority, and both
    deep direct anchors are simply outvoted without ever being touched.

    Odd count fixes the **deadlock**; it does not fix a **cheap majority**.
    The governing rule is:

    > **The EXPENSIVE sources must themselves be ≥ ⌈n/2⌉.** A source's cost is
    > `min(leg depths)`, so a routed source with a thin USD leg is a cheap
    > source no matter how deep its headline OSMO pair is.

    `three_direct_plus_two_routed_denies_the_cheap_majority` validates the
    shape that actually holds: **3 direct stable anchors + 2 routed**, 800s
    window. Corrupting *every* cheap source (both thin USD legs) is only 2 of
    5 — a minority — and the rate stays exactly $1.00. To move that set an
    attacker must beat a deep single-pool anchor: ~12% of 100 B, not 1 B.

    **Deploy shape:** either 3 direct + 2 routed, or keep 3 routed sources but
    ensure their `<asset>/USDC` legs are genuinely deep. Vet those legs as
    carefully as the OSMO pairs — they are usually the thin ones and they set
    each routed source's real price.

  Still deployment-specific: the actual depths of *your* pools and their
  independence. The testnet pool ids were `<...>` placeholders, so I could not
  evaluate the specific set — but the evaluation is now a lookup, not a study.

**(R3) "Leg-pair correctness on-chain — the contract cannot verify a leg pool
actually trades the declared pair."** → **UNCHANGED (correctly a
deployment-time check).** The E2E confirms that *when the leg pools do trade the
declared pairs*, the composite is exactly right and a wrong/dead leg is
discredited or deviation-filtered. It does not (and cannot) remove the operator
duty to verify each pool id off-chain before proposing — that remains a
pre-deploy checklist item, and the deviation filter + quorum are the on-chain
backstop if one is wrong.

**(R2-C / R3, router timelock and belief gate) — "real execution behind the
harness."** → **RESOLVED (clean).**
`router_e2e::router_timelock_belief_gate_and_no_wedge_end_to_end` runs the whole
router story on the **production factory bytes** (real 48h =
172,800s timelock; the chain clock is advanced with `increase_time`, so this is
the prod constant, not a shortened test build):
  - A direct null-`belief_price` `SimpleSwap` is **refused** (`belief_price is
    required`) when no router is registered (F-1 fail-closed); a belief-priced
    swap works; a **too-tight belief price makes the real poolmanager swap
    revert** on its `token_out_min_amount` floor and returns the funds.
  - A post-threshold `Commit` without `belief_price` is refused (H-3).
  - `ProposeRouter` is admin-only; a **pending** proposal grants **no
    exemption** (a route attempted during the window still dies at hop 0);
    `ApplyRouter` **before** the window is refused (`not yet effective`); after
    `increase_time(48h)` it lands and `RegisteredRouter` reflects it.
  - The **registered** router then routes creatorA→OSMO→creatorB end to end
    (two real `MsgSwapExactAmountIn` on seeded native pools) and delivers
    ≥ `minimum_receive`.
  - An impossible `minimum_receive` **reverts the whole multi-message route**
    (already-executed hop swaps roll back, the input returns, nothing stranded
    on the router), and **the very next route succeeds** — the F-5
    `ROUTE_IN_PROGRESS` guard does not wedge after a real on-chain revert. This
    is the F-5 "no-wedge" property proven with real rollback, end to end,
    beyond the cw-multi-test unit.

---

## Methodology note you should know about (affects how to read the oracle tests)

The oracle tests call `isolate_pricing_pools(app)` before creating pools, which
**disables `x/protorev` and zeroes the poolmanager taker fee**. This is not
hiding anything — it is isolating the unit under test. On a stock chain both
mechanisms *couple unrelated pools to the OSMO/USDC anchor*: protorev backruns
any dislocation through it, and non-OSMO taker fees are auto-swapped to OSMO
through it. Left on, a manipulation swap intended to move one leg *also* moves
the primary pricing pool, making an isolated assertion non-deterministic.
Disabling them lets each test move exactly one pool and assert an exact median.

The flip side is the F-4-relevant finding above: **on mainnet those mechanisms
are ON**, and protorev in particular is a *defensive* factor against oracle
manipulation that the prior F-4 analysis did not account for.

---

## Separated summary

**Confirmed and fixed this pass**
- E2E-1: `factory.wasm` failed cosmwasm-check (101 locals) → boxed the oracle
  field; would otherwise have blocked StoreCode. New artifact hashes above.
- E2E-2: the harness cross-denom-fee test silently ignored its fee argument →
  fixed; USDC-fee config path now actually covered.

**Verified clean under real execution (previously unprovable)**
- Crossing atomicity: real mid-chain failure rolls back mints + fee sends +
  ledger + threshold flag + returns funds; no wedge; POOL_ID from the real
  reply.
- Median oracle: exact live-TWAP median; dead/young/absurd/manipulated pools
  discredited; quorum fail-closed reverts commits taking nothing; duplicate
  rejected.
- Routed 2-leg pricing: exact composite across 8- and 6-decimal intermediates
  (decimals cancel, confirmed live); dead-leg discredited; median-bounded under
  single-leg manipulation.
- Cross-denom USDC fee crossing: full swap→create→charge chain, zero dust.
- Router: belief gate, pending-not-exempt, 48h timelock on prod bytes,
  `minimum_receive`, F-5 no-wedge — all end to end.

**Quantified this pass (previously "needs mainnet pool ids")**
- **F-4 cost law:** corrupting a source by +25% costs **~12% of its quote-side
  reserve**, scale-invariant across a 100× depth range. Evaluate any pool from
  one explorer lookup.
- **F-4 measured impact:** single-source config ⇒ attacker gets **~4× the
  airdrop weight** for the same OSMO ($3,980 vs $1,000 credit). Median config
  ⇒ **zero effect**. The exploitable config is the *default*.
- **R2-D boundary:** median holds at 1/5 and 2/5 corrupted, **flips at 3/5**,
  where the deviation filter ejects the honest minority. Attack budget =
  12%-of-depth summed over your cheapest ⌈n/2⌉ sources.
- **R2-E:** **no depth gate exists.** A 1000×-thinner pool is a full vote; a
  99% liquidity drain is completely invisible on-chain while cutting
  manipulation cost ~100×. But with **7 sources one thin pool discounts the
  attack only ~25%** (it is one of the four votes needed) — a minor weakness,
  not a hole. Four thin sources would be fatal.
- **No snapshot / no cache:** the rate is recomputed from
  `arithmetic_twap_to_now` on every valuation. The defence is the **TWAP
  window**, measured as a linear ramp ($1.00 at 0s held → $3.97 at the full
  300s). `twap_window_seconds` is a direct security dial.

**⚠ Action required on the intended pool set**
- The specified set (1 direct + 3 routed) is **4 votes, not 7**. Even count ⇒
  a 2/2 split discredits everything and **bricks all pricing** for the cost of
  two thin USD legs (~2 B against 600 B of untouched deep liquidity).
- Odd count alone is **not enough**: 2 direct + 3 routed still falls at 3/5
  for 3 × thin-leg capital, because the routed sources are a cheap majority.
- **Rule: the expensive sources must be ≥ ⌈n/2⌉.** Validated shape is
  **3 direct + 2 routed** (or 3 routed with genuinely deep USD legs).
  A source costs `min(leg depths)`, not its headline pair's depth.

**Still open (need owner inputs / decisions, not code fixes)**
- The **specific depths/independence** of your chosen pool set — the general
  laws are settled; plugging in your pools needs the real ids (were `<...>`).
  Verify each leg pair off-chain before proposing.
- **Ship-blocking config decision:** populate `oracle.extra_sources` (odd
  count, deep, independent). An empty set is the measured-exploitable path.
- **Optional hardening:** an on-chain per-source minimum-liquidity gate (R2-E)
  does not exist; today depth is guarded only by the RUNBOOK alarm.
- F-2 no pre-threshold committer exit — the brick is recoverable and loses no
  funds, but committer funds are *locked* during one; owner decision on a
  monitoring + exit-path answer.

**New, previously-unmodeled behavior worth folding into the risk docs**
- `x/protorev` in-protocol arbitrage partially self-corrects pricing-pool
  manipulation on mainnet — a mitigating factor for F-4.

---

## Phase 2 — real osmo-test-5 deploy: NOT run, and why

Per the task's own gate ("only if I've given you a funded key + RPC … otherwise
stop after Phase 1 and tell me what you need"), Phase 2 is blocked on two
missing inputs:

1. **The oracle pool ids/denoms are placeholders.** The task listed the intended
   set (USDC/OSMO primary, and routed BTC/ATOM/AKT with their USD legs) as
   `<...>` "I'll fill these in." `osmo_testnet.env` carries only the single
   primary (`PRICING_POOL_ID=314`, one `USD_QUOTE_DENOM`). The centerpiece of
   Phase 2 — wiring `oracle.extra_sources` (median + routed) into the
   instantiate payload — cannot be done without those ids, and I will not invent
   pool ids for a real deploy.
2. **The throwaway key is thin.** `FROM=alice`
   (`osmo192xm4dql0wzs727lae96td9ffuvxadam342efr`) holds **~26 OSMO** on
   osmo-test-5. That clears `MIN_GAS_BALANCE` but is marginal for a fresh
   deploy: store three (now larger) wasms + instantiate factory + router + walk
   a full crossing at the $20 threshold (~22 OSMO of commits that seed the pool
   and don't return) + the 1-OSMO gamm fee + gas. Prior full rehearsals topped
   alice to ~70 OSMO.

**To run Phase 2, I need from you:** the seven testnet pool ids + their denoms
(primary is 314; the three routed sources each need a native/volatile pool id
and a volatile/USD leg pool id + usd denom/decimals), and a top-up of the
alice key to ~60–80 OSMO (faucet):

```
osmo192xm4dql0wzs727lae96td9ffuvxadam342efr
```

With those I will show you the exact `FactoryInstantiate` JSON (with
`oracle.extra_sources` wired) before broadcasting anything, deploy the
**post-fix** bytes, and walk the lifecycle + live-oracle queries as specified.

Note that Phase 2 is now mostly a *confirmation* step: the oracle's security
properties were settled locally (above). What testnet adds is the real pool
ids, real IBC denoms, and the deploy-script wiring — not new security
information.

---

## Files

- New: `integration-tests/tests/oracle_e2e.rs`,
  `integration-tests/tests/oracle_economics_e2e.rs`,
  `integration-tests/tests/crossing_atomicity_e2e.rs`,
  `integration-tests/tests/router_e2e.rs`,
  `integration-tests/tests/common/mod.rs`.
- Changed: `factory/src/state.rs` (box the oracle field — the E2E-1 fix),
  `factory/src/testing/oracle_tests.rs` (`Box::new`),
  `integration-tests/tests/lifecycle.rs` (E2E-2 fee-param fix + an F-1
  belief-gate assertion), `integration-tests/Cargo.toml` (add the `router` dep).
- Re-optimized artifacts under `artifacts/` (hashes above); nothing pushed to
  git; no mainnet file touched.

**Run:** `cd integration-tests && LIBCLANG_PATH=… BINDGEN_EXTRA_CLANG_ARGS=…
cargo +stable test --release -- --test-threads=1` (env vars per
`integration-tests` build notes; the crate is workspace-excluded).
