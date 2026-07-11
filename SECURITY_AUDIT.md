# Bluechip Contracts — Security Review

**Scope:** all production CosmWasm contracts — `factory` (including the
x/twap USD-pricing module `usd_price`), `creator-pool`, `router` — and
the shared `pool-core` / `pool-factory-interfaces` libraries. The off-chain `keepers/` and build/release tooling were
reviewed at the configuration level.

**Method:** multiple independent review passes — verification of every
security control with file-level evidence, per-hunk adversarial review
of the fund-moving paths, a line-by-line deep dive on the x/twap
pricing route, and a residual-risk / test-coverage pass — with every
finding re-verified directly against source.

**Evidence baseline:** full workspace suite **377 tests, 0 failures**;
`cargo clippy --workspace --tests -- -D warnings` clean; release wasm
builds verified; CI enforces feature-clean production artifacts
(`ci/check_prod_build.py` + Makefile hard-fail).

**Threat model:** the factory admin is trusted; the adversary is any
unprivileged caller (committer, trader, LP, keeper-caller). The only
CW20 inside any pool is the vanilla cw20-base token the factory itself
mints, so no third-party token code executes in the system. Pool
creation is permissionless.

---

## Headline result

No Critical, and no unprivileged-exploitable High, in any on-chain
contract. The codebase is hardened where it counts: checked / `Uint256`
arithmetic throughout, checks-effects-interactions ordering on every
fund-moving path, four independent gates on the threshold crossing, and
conservation invariants that hold under adversarial tracing. The open
items below are Low/Info, consciously accepted economic designs, or
operational duties.

## Open findings

| ID | Sev | Area | Finding |
|----|-----|------|---------|
| S-1 | Low | pricing | No on-chain liquidity/depth floor on the pricing pool. `x/twap` never reports "stale": a draining pool keeps returning prices while the cost of manipulating them silently falls. **Mitigation is operational** — alarm on `pricing_pool_id` liquidity and re-point it via the 48h config flow if OSMO/USDC depth ever migrates (see `RUNBOOK.md`). The `RATE_MAX` ceiling bounds the upside of any spike. |
| S-2 | Info | pricing | Arithmetic (not geometric) TWAP, window floor 300s (default 600s). Arithmetic means upward spikes contribute linearly — the attacker-profitable direction. Consider `GeometricTwapToNow` if hardening further. |
| S-3 | Med (economic) | creator-pool | The creator's 325k-token allocation is unvested at crossing, and creators may commit to their own pools (self-fundable threshold). Accepted design; consider vesting before mainnet — this is the first question a reviewer will ask. |
| S-4 | High (ops) | governance | Until the factory admin, contract (migration) admin, and `PROTOCOL_WALLET` are a multisig, a single leaked key controls the protocol, and every in-contract 48h timelock is advisory. **Pre-mainnet requirement** — see `docs/MULTISIG.md`. |
| S-5 | Low | migrate | Migrate handlers enforce semver downgrade protection but not the cw2 contract-*name*; and the router has **no migrate entry point** (changing it means redeploying and re-pointing integrators). |
| S-6 | Low | queries | `CumulativePrices` reads live balances (which include fee reserves and pots), so the externally-consumable TWAP tail is donation-manipulable. `Simulation` / `ReverseSimulation` quote tracked reserves and are unaffected; no internal consumer reads the accumulator. Integrators should not price off `CumulativePrices` without depth checks. |
| S-7 | Low | verification | Coverage gaps: the pricing fixed-point math (`twap_dec_to_rate` / `native_to_usd` / `usd_to_native_at_rate`) is unfuzzed (`fuzz_threshold_check` models it only approximately); there is no stateful property harness over the commit → threshold → swap/liquidity lifecycle; and creator-pool tests pin the USD rate at 1:1, so no scenario varies the rate mid-lifecycle. See `FUZZING.md`. |
| S-8 | Info | deploy | `PROTOCOL_WALLET` defaults to the deployer and the contract admin moves to the multisig only post-instantiate — a deploy-key-as-admin window. `deploy_osmosis.sh` refuses mainnet deploys without an explicit `PROTOCOL_WALLET`; keep the window short. |
| S-9 | Info | events | `ContinueDistribution` emits a `bounty_paid` attribute even though no bounty mechanism exists (always `false`); cosmetic, but indexers should not key on it. |

## Verified defenses (with the properties that were checked)

**Pricing (factory `usd_price`)**
- Fail-closed end-to-end: any TWAP query error, zero/dust price, or
  price above the `RATE_MAX` **$10,000-per-native sanity ceiling**
  reverts the valuation — a commit that cannot be priced correctly
  cannot be priced at all. The ceiling also catches a wrong-decimals
  quote denom (an 18-decimal stable inflates the rate ~1e12×).
- Live probe of the candidate pricing route at **instantiate, propose,
  and apply** (`validate_factory_config` →
  `usd_price::probe_native_usd_rate`): a typo'd `pricing_pool_id`, a
  pool missing a denom, or a pool younger than the window fails
  instantly instead of as a chain-wide commit outage. Regression tests:
  `instantiate_rejects_dead_pricing_route`,
  `propose_rejects_dead_pricing_route`, `apply_reprobes_pricing_route`,
  `propose_rejects_wrong_decimals_quote_rate`,
  `rejects_rates_above_sanity_ceiling`.
- Rounding is floor-directed **against the committer** on both the rate
  and the valuation; the crossing's inverse conversion reuses the
  captured rate, so ledger/threshold drift is ≤1 base unit and reverts
  via checked arithmetic rather than misallocating. No price is cached;
  there is no update cadence to manipulate.

**Threshold crossing (creator-pool)**
- One-shot behind four independent gates: the dispatcher's
  `THRESHOLD_PROCESSING` latch and `IS_THRESHOLD_HIT` routing, fail-fast
  entry gates in both crossing handlers, the load-bearing
  `IS_THRESHOLD_HIT` check-then-set in `trigger_threshold_payout`, and
  the factory-side `POOL_THRESHOLD_CROSSED` idempotency flag (gated on
  the registered pool as sender).
- Payout components are pinned to canonical constants and validated on
  both sides; the CW20 mint cap equals the **exact** payout total
  (1.2M tokens), so over-mint fails closed at cw20-base. Ledger
  conservation holds on exact-hit and excess paths.
- The crossing transaction's excess swap is capped at **3% of
  reserves** with the remainder **refunded**; a 5% spread guard applies;
  a 2-block cooldown plus a 100-block per-tx swap-cap ramp
  (0.5% → 100%) bounds post-crossing MEV.

**Commit path**
- Denom validation is triple-gated (asset-info equality,
  `bluechip_denom` match, `must_pay` exact amount) — no
  worthless-denom credit path. One USD rate is captured per transaction
  and threaded through every conversion. Floors: $5 pre-threshold /
  $1 post (admin ceiling $1,000). 13s per-wallet rate limit.

**LP / liquidity (pool-core)**
- Fee-growth checkpoint accounting prevents pre-deposit and double
  claims; `CollectFees` pays without touching the position. Every
  position op is NFT-ownership-gated.
- Liquidity operations use a **dedicated cooldown map keyed on the real
  signer**, so a hostile CW20's `Receive` hook cannot stamp an LP's
  withdrawal cooldown (regression:
  `swap_and_liquidity_rate_limits_use_independent_maps`).
- First deposit requires BOTH credited sides ≥ `MINIMUM_LIQUIDITY` and
  locks 1000 LP units unwithdrawably (regression:
  `first_deposit_rejects_subfloor_reserve_side`); reserves are
  internally tracked, so donations cannot inflate share price; removals
  below the floor auto-pause the pool.
- Emergency withdraw is two-phase (config-set delay) with LP shares
  escrowed for a 1-year claim window; claims hard-close after the sweep
  so the snapshot ledger cannot go inconsistent. Drains route to the
  protocol wallet, never the factory.

**Router**
- Every hop's pool address is validated against the factory registry —
  and its declared (offer, ask) against the pool's real sides — before
  any funds move (regressions: `route_through_unregistered_pool_rejected`,
  `route_with_mislabeled_pair_rejected`). `minimum_receive = 0` is
  rejected (`router_rejects_zero_minimum_receive`); per-hop `max_spread`
  is pinned to the pools' 5% hard cap so `minimum_receive` is the
  binding end-to-end slippage control. Simulations quote tracked
  reserves and error cleanly on zero-reserve pools
  (`simulation_on_zero_reserves_errors_cleanly_instead_of_panicking`).

**Factory governance surface**
- Every privileged `ExecuteMsg` is admin-gated; the permissionless
  surface is exactly {`Create` (flat fee +
  1h/address rate limit), `NotifyThresholdCrossed` (registered-pool
  sender + idempotent), `PruneRateLimits` (batch-clamped)}. All
  config / pool-config / upgrade flows are 48h propose→apply with no
  early-apply, no replay, and no silent overwrite of a pending
  proposal. Reply-chain IDs are structurally unforgeable; pair
  uniqueness is enforced at registration.

**Build hygiene**
- Contract crates ship `default = []`; the deployable factory artifact
  is the feature-empty `prod` optimizer build, enforced by a Makefile
  hard-fail and CI's `prod-artifact-guard`; the `integration_short_timing`
  test feature cannot reach a shipped artifact.

## Operational requirements

Codified in `RUNBOOK.md`: the once-a-minute `ConvertNativeToUsd` canary
probe, a liquidity-floor alarm on the pricing pool (S-1), the
distribution keeper under supervision, and calendared two-step
execution of every 48h timelock.

## Priorities

1. **S-4** — multisig for admin/migration/treasury before mainnet
   (`docs/MULTISIG.md`).
2. **S-3** — an explicit vesting decision on the creator allocation.
3. **S-1 + S-7** — stand up the pricing-pool liquidity alarm; restore
   fuzz/property coverage over the pricing math and the
   commit→threshold→swap/liquidity lifecycle.
4. **S-5/S-6** — cheap hardening: cw2 name check on migrate, a router
   migrate entry point, tracked-reserve sourcing for `CumulativePrices`.
