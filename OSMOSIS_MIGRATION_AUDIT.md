# Osmosis Migration — Security Audit Delta

**Scope:** the full migration diff `bluechip-contracts/main (0adb0b4)` →
`bluechip-osmosis-contract HEAD (183fd4a)` — 161 files, +3,606/−25,930 —
plus a line-by-line review of the new `factory/src/usd_price.rs`, a
defense-parity check of every control documented in `SECURITY_AUDIT.md` /
`AUDIT_DELTA.md`, and a residual-risk / coverage-regression pass.

**Method:** four independent review passes (defense parity; creator-pool +
pool-core diff; factory + router + x/twap pricing; residual risk & test
coverage), every finding verified against source. Full workspace suite at
HEAD: **458 tests passed, 0 failed, 0 ignored**; `cargo check --workspace
--tests` clean.

**Question answered:** is the new contract as well defended as the old one?

---

## Headline result

**All previously-audited defenses survived the migration intact.** Every
fixed finding from the prior audits (F-1, F-2, F-9, DA-1, DA-2, H-1) is
present in the new code with its regression test; the threshold
no-double-cross gates (4 independent), checked/Uint256 arithmetic,
checks-effects-interactions ordering, MINIMUM_LIQUIDITY lock, factory admin
gating (all 16 privileged handlers), 48h timelock apply/replay protection,
router registry validation, and the 5% spread hard caps are all verified
unchanged. The router diff is 100% formatting. No new unchecked arithmetic
was introduced on any fund path.

**The migration also removes whole attack-surface classes:** ~20 oracle
ExecuteMsgs are gone, there is no cached price and no update cadence to
manipulate (old F-5 and M-2 are structurally impossible now), the keeper
bounty-farming vector is deleted, and old F-4's mint-schedule anchor is
moot (mint machinery removed; `crossed_at` is event-only).

**However, the new codebase is *not yet* at full parity in three respects:**

1. **Price-path defense-in-depth was removed wholesale, not replaced.**
   The old oracle had a ~30%-per-round drift circuit breaker plus a 120s
   staleness gate. The new x/twap path is fail-closed on errors — good —
   but accepts *any* nonzero rate: no upper bound, no deviation clamp, and
   no liveness/depth check on the pricing pool (a drained Osmosis pool
   keeps returning a "valid" TWAP silently).
2. **One new Medium (F-N1):** the pricing config (`pricing_pool_id`,
   `usd_quote_denom`) is only syntactically validated. A typo'd pool id
   passes propose *and* apply, and the failure surfaces as a chain-wide
   commit outage that takes another 48h timelock to fix.
3. **Verification muscle was lost on still-alive code:** the stateful fuzz
   harness (which found F-9) was deleted; the live pricing math and the
   `query_native_usd_rate` entry point have zero test coverage; every
   test that varied the price mid-lifecycle was deleted (the mock now pins
   the rate at 1:1 forever).

---

## New findings (this migration)

| ID | Sev | Title | Location |
|----|-----|-------|----------|
| F-N1 | **Medium** | Pricing config never probed on-chain at propose/apply; a bad `pricing_pool_id`/denom bricks every commit chain-wide for ≥48h (fail-closed outage, 48h timelock to repair). **Fixed** — see "Remediations applied" | `factory/src/execute/config.rs` |
| F-N2 | Low | 6-decimal assumption on `bluechip_denom`/`usd_quote_denom` is unenforced; a wrong-decimals quote asset misprices commits by ~1e12× (fail-open). Admin-gated + timelocked. **Mitigated** — the RATE_MAX ceiling now rejects the inflated rate at probe time and on every commit | `factory/src/usd_price.rs` |
| F-N3 | Low | No rate sanity clamp and no liquidity/staleness floor on the pricing pool. The old 30%-drift breaker + staleness gate have no analog; manipulation cost silently decays if OSMO/USDC liquidity ever migrates off the configured pool. **Partially fixed** — RATE_MAX caps the upside of any spike/misconfig; an operational liquidity alarm is still recommended | `factory/src/usd_price.rs` |
| F-N4 | Low | Coverage regression: `fuzz-stateful/` deleted (found F-9; modeled still-alive flows); `twap_dec_to_rate`/`native_to_usd`/`usd_to_native_at_rate` unfuzzed (`fuzz_threshold_check` still models retired Pyth math); no TwapQuerier mock, so `query_native_usd_rate` is untested; 6–7 rate-variation tests deleted without replacement | `fuzz/`, `factory/src/mock_querier.rs`, `creator-pool/src/mock_querier.rs:103-112` |
| F-N5 | Low | Documented deploy/ops path is not executable: `deploy_osmosis.sh` / `deploy_robust.sh` / `scripts/verify_deploy.sh` do not exist in the tree; RUNBOOK's health probe targets the deleted `internal_blue_chip_oracle_query`; no monitoring guidance exists for the new single price dependency. **Fixed** — `deploy_osmosis.sh` added (store + instantiate + ConvertNativeToUsd verification probe; its instantiate JSON is pinned by the `deploy_script_instantiate_json_deserializes` regression test); RUNBOOK rewritten for the x/twap world (new canary probe + pricing-pool liquidity alarm); keeper `.env.example` moved to Osmosis values | `deploy_osmosis.sh`, `RUNBOOK.md`, `keepers/.env.example` |
| F-N6 | Info | 60s minimum TWAP window (a single block carries 2.5–7% weight at the floor) and arithmetic (not geometric) TWAP — spike-sensitive in the attacker-profitable direction. Default 600s is sound. **Partially fixed** — floor raised to 300s; arithmetic TWAP retained (geometric left as a future consideration) | `factory/src/usd_price.rs` |
| F-N7 | Info | Creation fee changed from USD-pegged to flat native — anti-spam friction floats with OSMO price (deliberate; rate limits backstop) | `factory/src/execute/pool_lifecycle/create.rs` |
| F-N8 | Info | Mainnet env fail-opens to deploy-key ownership: `PROTOCOL_WALLET=""` defaults to deployer; contract admin moves to the multisig only post-deploy | `osmosis_mainnet.env:66-68`, `docs/OSMOSIS_DEPLOY.md:117-118` |
| F-N9 | Info | Migration leftovers: `"bounty_paid"` event attribute emitted though no bounty exists (`creator-pool/src/commit/distribution.rs:93`); stale comments referencing removed oracle/mint machinery in ~10 files; `InvalidOraclePrice` error variant still live on the commit path; distribution liveness now depends entirely on the protocol-run keeper (unpaid `ContinueDistribution`) while RUNBOOK still describes the bounty model | various |

## Old open findings — status at HEAD

| ID | Old status | New status |
|----|-----------|------------|
| F-3 (balance-lying CW20 drains own pool; docs overclaim "hostile-CW20 safe") | Open (decision) | **Docs remediation taken** — README rewritten with an explicit "Standard-pool CW20 risk" disclosure (pool-isolated drain by a balance-lying token; LPs must trust the CW20). The code-side option (token allowlist) remains not implemented, by decision |
| F-4 (`crossed_at` anchors mint schedule) | Open | **Moot** — mint machinery deleted; `crossed_at` is event-only (`factory/.../admin.rs:226-246`). Stale doc-comment at `:182-183` |
| F-5 (oracle cooldown bypass) | Open | **Moot** — no cached price, no update cadence exists |
| F-6 (staleness gate fail-open on `timestamp==0`) | Open | **Moot as written**; new path is fail-closed end-to-end. The x/twap-shaped analog (no liveness gate on the pricing pool) is F-N3 |
| F-7 (query pricing vs execution pricing) | Open | **Partially fixed** — Simulation/ReverseSimulation now quote tracked reserves (AUDIT_DELTA C5), but `query_cumulative_prices` still reads live balances (`packages/pool-core/src/query.rs:132-154`): donation-manipulable TWAP tail remains for external integrators |
| M-1 (unlocked 325B creator allocation; self-fundable threshold) | By design | **Still applicable, unchanged** (`creator-pool/src/commit/threshold_payout.rs:224-228`; no self-commit restriction) |
| M-2 (post-reset TWAP dilution) | By design | **Moot** — resettable oracle gone |
| M-4 (pre-anchor flat fallback fee) | By design | **Superseded** — fee is now flat-native by design (see F-N7) |
| I-1 (single-EOA migrate+admin key) | Accepted | **Still applicable** — governance gates only mainnet wasm upload; every in-contract timelock remains advisory until the admin key is a multisig |
| L-1 (no cw2 contract-name check on migrate) | Accepted | **Still applicable** — all three migrate handlers check semver only. Additionally: **router has no migrate entry point at all** (unupgradeable without redeploy) |

## Verified clean (high-value confirmations)

- **Commit denom validation is triple-gated** (`creator-pool/src/commit.rs:173,217,234`):
  asset-info equality, `bluechip_denom` match, and `must_pay` exact-amount
  check — no worthless-denom credit path. Swap and liquidity entry points
  validate natives equivalently (`asset.rs:62-70`, `liquidity/deposit.rs:150-181`).
- **Price flow fail-closed end-to-end:** factory TWAP error / zero /
  sub-1e-6 rate → `Err` (`usd_price.rs:57-91`); pool propagates the error →
  commit reverts (`swap_helper.rs:40-48`); pool re-checks zero
  (`commit.rs:189`). No fallback price anywhere.
- **Rounding rounds against the committer** on both the rate floor and the
  valuation floor; the threshold-crossing inverse conversion reuses the
  captured rate, so ledger/threshold drift is ≤1 base unit and reverts via
  `checked_sub` rather than misallocating.
- **Conservation:** ledger sum ≤ threshold with exact-hit and excess paths
  reconciling; 1.2T payout cap enforced component-wise and in total;
  distribution uses Uint256 floor division with dust-to-creator only when
  under-distributed; `COMMIT_LEDGER` entries removed before mint submsgs
  dispatch (CEI).
- **Factory:** all privileged handlers `ensure_admin`-gated; permissionless
  surface is exactly {Create, CreateStandardPool, NotifyThresholdCrossed
  (registered-pool-sender + idempotent), PruneRateLimits (clamped 500)};
  reply IDs unforgeable; timelocks have no early-apply/replay.
- **Build hygiene (H-1)** holds: `default = []`, prod optimizer profile,
  Makefile hard-fail, CI prod-artifact-guard; the `mock` feature no longer
  exists anywhere. CI retains fmt, clippy `-D warnings`, cargo-deny
  advisories, workspace tests, wasm builds (standard-pool build added).

## Remediations applied on this branch

- **F-N1 fixed:** `validate_factory_config` now takes `Env` and ends with a
  live `usd_price::probe_native_usd_rate` call against the candidate
  config, running at **instantiate, propose, and apply** (apply re-probes
  so a pool that dies during the 48h window is caught before the config
  lands). A typo'd `pricing_pool_id`, a pool missing one of the denoms, or
  a pool younger than the window now fails instantly instead of as a
  chain-wide commit outage. Regression tests:
  `instantiate_rejects_dead_pricing_route`,
  `propose_rejects_dead_pricing_route`, `apply_reprobes_pricing_route`.
- **F-N2/F-N3 mitigated:** `twap_dec_to_rate` now enforces `RATE_MAX`
  ($10,000 per native token) in addition to the zero/dust floor — the
  stateless replacement for the old oracle's drift breaker. A
  wrong-decimals quote denom (~1e12× inflation) or a spiked pricing pool
  is refused both at config-probe time and on every commit valuation.
  Regression tests: `rejects_rates_above_sanity_ceiling`,
  `propose_rejects_wrong_decimals_quote_rate`.
- **F-N6 partially fixed:** `TWAP_WINDOW_MIN_SECONDS` raised 60 → 300 (all
  shipped configs already used 600).
- **F-N4 partially addressed:** the factory mock querier now answers the
  x/twap Stargate query (`twap_result`, default $1.00, with
  `set_twap_price` / `set_twap_error` overrides), so the live pricing
  entry point is exercisable in tests; the config-probe regression tests
  above use it. The deleted stateful fuzz harness and rate-variation
  scenario tests remain to be restored.

## Priority recommendations

1. ~~**F-N1:** live-probe the proposed pricing config~~ — **done** (above).
2. **F-N2/F-N3 (residual):** consider `GeometricTwapToNow`, and add an
   operational alarm on pricing-pool liquidity depth; the RATE_MAX clamp
   and 300s window floor are in place.
3. **F-N4 (restore verification muscle, residual):** re-point
   `fuzz_threshold_check` at the live
   `twap_dec_to_rate`/`native_to_usd`/`usd_to_native_at_rate` math; make
   the creator-pool mock rate configurable and reinstate rate-variation /
   rounding-accumulation scenarios; ideally resurrect the stateful harness
   for the still-alive commit→threshold→swap/liquidity flows. (The factory
   TwapQuerier mock is now in place.)
4. **F-N5 (ops):** add the missing deploy/verify scripts (or fix the docs),
   and rewrite RUNBOOK monitoring around the new dependency — probe
   `PoolFactoryQuery::ConvertNativeToUsd` and alarm on pricing-pool
   liquidity depth.
5. **Docs honesty pass:** update SECURITY_AUDIT.md finding statuses
   (F-4/F-5/F-6 moot, F-7 partial), fix test counts (458), add the
   strip-down notice to AUDIT_DELTA.md, purge README's oracle /
   expand-economy / bounty / USD-fee sections, and make the F-3 decision
   (allowlist or disclose).
6. **Pre-mainnet (unchanged from the old audit):** multisig/governance for
   the migrate+admin key (I-1); vesting decision on the creator allocation
   (M-1); minimize the deploy-key-as-admin window (F-N8).
