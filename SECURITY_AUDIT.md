# Bluechip Protocol — Security Audit

**Scope:** all production CosmWasm contracts — `factory` (including the
~2,860-line internal price oracle), `creator-pool`, `standard-pool`,
`expand-economy`, `router`, and the shared `pool-core` /
`pool-factory-interfaces` libraries. Off-chain `keepers/` and the
build/release tooling were reviewed at the configuration level.

**Method:** five independent line-by-line passes (one per subsystem), every
finding re-verified directly against source. The full workspace compiles
clean; the deterministic test suite is green (644 tests). One stateful-fuzz
invariant violation was reproduced on the clean tree and is recorded below
as F-9.

**Threat model:** factory admin / keepers / Pyth publishers are trusted;
the adversary is any unprivileged caller (committer, trader, LP,
keeper-caller) and, for standard pools, a hostile CW20 token contract. Pool
creation is permissionless.

---

## Headline result

No Critical, and no unprivileged-exploitable High, was found in any on-chain
contract. The codebase is genuinely hardened: checked / `Uint256` arithmetic
throughout, checks-effects-interactions ordering on every fund-moving path,
multiple independent no-double-mint gates on the threshold crossing, and
conservation invariants that hold under adversarial tracing. The prior
build-hygiene gap (H-1) is remediated and verified.

Two Medium findings reachable by an unprivileged actor — **F-1** (hostile
CW20 freezing an LP's withdrawals) and **F-2** (router routing to an
unvalidated pool address) — are **fixed on this branch**. The remainder are
Low / Informational or by-design economic decisions.

---

## Findings summary

| ID | Severity | Area | Status |
|----|----------|------|--------|
| F-1 | Medium | pool-core | Hostile CW20 spoofs `cw20_msg.sender` to freeze a victim LP's withdrawals via a shared rate-limit map. **Fixed.** |
| F-2 | Medium | router | Hop `pool_addr` never validated against the factory registry; malicious-frontend fund loss. **Fixed.** |
| F-3 | Medium→Info | standard-pool | Balance-verify is read *from the token itself*, so a balance-lying CW20 can drain *its own pool's* paired asset. Pool-isolated; docs overclaim "hostile-CW20 safe." **Open (decision).** |
| F-4 | Low | factory | `NotifyThresholdCrossed.crossed_at` has no lower bound; first crossing anchors the global mint-decay schedule. **Open.** |
| F-5 | Low | oracle | `UpdateOraclePrice` 60s cooldown is bypassed during the post-reset buffer (`last_update` stays 0). **Open.** |
| F-6 | Low | creator-pool | Oracle staleness gate fail-opens when the factory returns `timestamp == 0`. **Open.** |
| F-7 | Low | pool-core | `Simulation` / `CumulativePrices` queries price against live balances, not tracked reserves. **Open.** |
| F-8 | Low (ops) | keepers | Default `ORACLE_POLL_INTERVAL_MS=330s` contradicts the on-chain 120s staleness gate. **Open.** |
| F-9 | Low–Med | pool-core | First deposit with a highly asymmetric ratio could leave one reserve below `MINIMUM_LIQUIDITY` (found via the stateful fuzz harness; pre-existing). **Fixed.** |
| — | Info | various | Dead ungated oracle getters; doc/const mismatches; unbounded never-pruned maps; broken `optimize-pool` Makefile target; stale committed `*.wasm` blobs; "expand-economy" disburses from a pre-funded reservoir (not a literal mint). |

Carried forward from the prior pre-audit (by-design / accepted, unchanged):
**M-1** unlocked 325k creator allocation + self-fundable threshold (soft-rug
economics — consider vesting); **M-2** post-reset TWAP dilution; **M-4**
pre-anchor flat fallback fee; **I-1** single-EOA migrate+admin key (the
dominant caveat — every in-contract timelock is advisory until this becomes
a multisig/governance key); **L-1** missing cw2 contract-name check on
migrate.

---

## Fixed on this branch

### F-1 (Medium) — Hostile CW20 freezes a victim LP's withdrawals
`packages/pool-core/src/swap.rs`, `generic.rs`, `state.rs`,
`liquidity/{deposit,add,remove}.rs`

The CW20 swap path took the rate-limit identity from `cw20_msg.sender`
(`swap.rs`), a field the *token contract* constructs in its `Receive` hook.
For an arbitrary standard-pool CW20 that value is attacker-controlled. It was
written into the shared `USER_LAST_COMMIT` map, which the liquidity-removal
paths also read keyed on the real signer. A hostile token could therefore
stamp a victim's key every <13s and indefinitely block their
`RemoveLiquidity` with `TooFrequentCommits` — removing their only exit while
the attacker worked the paired side.

**Fix:** liquidity operations (deposit / add / remove) now use a dedicated
`USER_LAST_LIQUIDITY_OP` map keyed only on the real `info.sender`; swaps and
commits keep `USER_LAST_COMMIT`. No swap, spoofed or not, can ever stamp a
liquidity-op cooldown. Regression test:
`pool-core … generic::tests::swap_and_liquidity_rate_limits_use_independent_maps`.

### F-2 (Medium) — Router did not validate hop pool addresses
`router/src/execution.rs`, `factory/src/query.rs`,
`packages/pool-factory-interfaces/src/{lib,routing}.rs`

`validate_route` checked only the route's internal shape; execution
dispatched straight to the caller-supplied `pool_addr`, and the stored
`factory_addr` was dead code. A malicious frontend could route a user's
funds to an attacker contract with `minimum_receive` as the only backstop.

**Fix:** added a factory `PoolByAddress` registry query and a router step
that, for every hop, confirms the address is a registered pool and that the
declared `(offer, ask)` are that pool's two real sides — before any funds
move. `factory_addr` is now load-bearing. Regression tests:
`router … route_through_unregistered_pool_rejected`,
`route_with_mislabeled_pair_rejected`.

### F-9 (Low–Medium) — First deposit could leave a reserve below MINIMUM_LIQUIDITY
`packages/pool-core/src/liquidity_helpers.rs`

`calc_liquidity_for_deposit` floored only the geometric mean
(`sqrt(amount0·amount1) > MINIMUM_LIQUIDITY`) on the first deposit, so a
highly asymmetric seed such as `(20, 500_000_000)` passed
(`sqrt(20·5e8) = 100_000`) yet left `reserve0 = 20` — below the floor the swap
path and `maybe_auto_pause_on_low_liquidity` both assume every live pool
upholds, leaving the pool swap-broken on one side. Surfaced by the stateful
fuzz harness (`minimum_liquidity_breached`); it predated and was not
introduced by F-1/F-2.

**Fix:** the genuinely-empty first deposit now requires BOTH credited amounts
`≥ MINIMUM_LIQUIDITY`. Verified by the fuzz harness that found it (now green),
a 3,000-case stateful run (clean), and a deterministic regression test
(`standard-pool … first_deposit_rejects_subfloor_reserve_side`).

---

## Open findings (recommended next)

### F-3 — "hostile-CW20 safe" is overstated for standard pools
The deposit/swap balance checks query the token's balance *from the token
contract*, so a fully hostile, balance-lying CW20 defeats them and can drain
the paired asset **within its own pool**. This is **pool-isolated** — it
cannot reach other pools or protocol funds (the swap only touches the one
pool whose `asset_infos` contain that token), so it is the standard
permissionless-AMM LP risk, not a Critical. But the README/pre-audit describe
standard pools as hostile-CW20-safe, which is inaccurate for the
balance-lying case. **Action:** either add a token allowlist for standard
pools, or correct the docs and disclose the residual LP risk.

### F-4 — Caller-supplied `crossed_at` anchors the global mint schedule
`factory/src/.../admin.rs`, `mint_bluechips_pool_creation.rs`. The clamp
rejects only future timestamps; the first crosser's `crossed_at` becomes
`FIRST_THRESHOLD_TIMESTAMP`, and larger elapsed time *increases* later pools'
mint. A buggy/compromised registered pool could push every later pool toward
the cap (bounded by the daily expansion cap; not unprivileged-reachable
today). **Action:** anchor to `env.block.time` or add a lower-bound clamp.

### F-5 — Oracle cooldown bypass in the post-reset buffer
`factory/src/internal_bluechip_price_oracle.rs`. After a reset `last_update`
is 0; the buffer branches early-return without advancing it, so the 60s
floor never fires and an attacker can call `UpdateOraclePrice` every block,
reaching the 12-failure force-accept in ~12 blocks instead of ~12 keeper
cycles. Bounded by warm-up (strict consumers stay frozen). **Action:** stamp
a `last_attempt_time` on every entry.

### F-6 — Staleness gate fail-opens on `timestamp == 0`
`creator-pool/src/swap_helper.rs`. The 120s gate is guarded by
`timestamp > 0 && …`; a live non-zero rate paired with `timestamp == 0`
(genesis/misconfig) skips the staleness check. Not unprivileged-reachable.
**Action:** treat `timestamp == 0` as stale (fail closed).

### F-7 — Query pricing diverges from execution pricing
`packages/pool-core/src/query.rs`. `Simulation` / `ReverseSimulation` /
`CumulativePrices` source pool size from live balances, which include
`fee_reserve + creator_pot + donations`, whereas swaps execute against
tracked reserves. External integrators (routers/feeds) see biased numbers and
a donation-manipulable TWAP tail. The protocol's own oracle reads tracked
reserves and is unaffected. **Action:** use `POOL_STATE.reserve*` in these
handlers.

### F-8 — Keeper default poll interval contradicts the staleness gate
`keepers/src/lib/config.ts`. `ORACLE_POLL_INTERVAL_MS` defaults to 330s
(sized for the retired 300s on-chain interval), but the pool-side staleness
gate is 120s. Default-configured keepers leave commit valuations stale-blocked
for most of each cycle. **Action:** lower the default to ≤90s.

---

## Verified clean (high-value confirmations)

- **Commit/threshold conservation:** ledger sum ≤ threshold; four
  independent no-double-cross gates; distribution ≤ 500k always; CW20 1.2M
  cap = exact sum of payouts (over-mint fails closed at cw20-base). Bank vs
  reserves reconcile on both exact-hit and excess paths.
- **Oracle math:** trapezoidal TWAP; correct price direction (no inversion);
  `PRICE_PRECISION`/`PRICE_ACCUMULATOR_SCALE` consistent on every
  multiply/divide; accumulator overflow fails closed; Pyth gate ordering is
  all-AND with negative/zero/`expo>0` rejected; cache bounded by
  `publish_time`. Strict-vs-best-effort bifurcation is complete across crates
  — no stale/warm-up price can leak into commit valuation.
- **Liquidity:** first-depositor `MINIMUM_LIQUIDITY` lock enforced and
  unwithdrawable; donations cannot inflate share price (reserves are
  internally tracked); fee-growth checkpoints prevent claiming pre-deposit or
  double fees; NFT ownership gates every position op; two-phase emergency
  withdraw + 1-year claim math cannot over-claim.
- **Factory:** every privileged `ExecuteMsg` is admin-gated; reply-chain IDs
  are unforgeable; timelocks have no early-apply/replay; the decay `x` is not
  attacker-inflatable.
- **expand-economy:** factory-only disbursement; denom never caller-
  controlled; correct 24h sliding-window cap; a failed/skipped payout does
  not burn budget.
- **Build hygiene (H-1):** remediation holds — empty default features + a
  `prod` empty-feature optimizer build + a Makefile hard-fail + a CI static
  guard; the `mock` / `integration_short_timing` features are confined to
  `factory` and `expand-economy`.

---

## Priority

1. **F-3 decision** — allowlist standard-pool tokens *or* correct the
   "hostile-CW20 safe" docs.
2. **F-5, F-6, F-8** — cheap fail-closed/hardening changes.
3. **Pre-mainnet (non-code):** multisig/governance for the migrate+admin key
   (I-1); a vesting decision on the unlocked creator allocation (M-1).
