# Delta Security Review — post-baseline changes

**Scope:** all contract-source changes on `claude/loving-albattani-eolo8l`
relative to merge-base `618fb9d` (the last externally-reviewed baseline;
see `SECURITY_AUDIT.md` for the full-codebase review that baseline had).
**Reviewed:** 2026-06-11. **Status of findings: 2 found, 2 fixed in this
pass** (commit referenced below), each with a regression test.

This is an internal adversarial review of the delta, performed before
handing the codebase to an external auditor. It is **not** a substitute
for that audit; its purpose is to make the external pass cheaper and to
document the reasoning behind every behavioral change since baseline.

## Methodology

- Exact diff enumeration (`git diff 618fb9d..HEAD -- '*.rs'`):
  10 production source files, 7 test/mock files.
- Per-hunk adversarial analysis against: authorization, state mutation,
  panics/DoS in query paths, gas boundedness, wire-format compatibility,
  arithmetic safety, and economic-incentive changes.
- Full workspace suite after fixes: **663 tests, 0 failures**; release
  wasm builds verified for all five contracts; `ci/check_prod_build.py`
  (feature-clean optimizer builds) passing.

## Change inventory

| # | Change | Files | Risk class |
|---|--------|-------|-----------|
| C1 | `CreatorEarnings {}` query (creator-pool) | `creator-pool/src/{msg,query}.rs` | Read-only, additive |
| C2 | `Pools { start_after, limit }` registry enumeration (factory) | `factory/src/query.rs` | Read-only, additive |
| C3 | CW20 `marketing` set at token instantiate, admin = pool creator | `factory/src/msg.rs`, `factory/src/execute/pool_lifecycle/create.rs` | Instantiate-path, additive |
| C4 | `last_committed` serde alias on `LastCommited` query | `creator-pool/src/msg.rs` | Wire, additive |
| C5 | Simulations quote from `POOL_STATE` accounting reserves | `packages/pool-core/src/query.rs` | **Behavioral** (read path) |
| C6 | Router simulation: registry-gated per-hop validation | `router/src/simulation.rs` | **Behavioral** (read path) |
| C7 | Router pins per-hop `max_spread` to the pools' 5% hard cap | `router/src/{execution,msg}.rs` | **Behavioral** (execution) |
| C8 | expand-economy rejects `factory_address == instantiator` | `expand-economy/src/{contract,error}.rs` | Instantiate-path guard |

## Findings

### DA-1 (Low, FIXED): zero-reserve simulation panicked the query VM
Introduced by C5. `compute_swap` computes
`Decimal256::from_ratio(ask_pool, offer_pool)`, which **panics on a zero
denominator**. After C5, simulations read `POOL_STATE` reserves — which
are `0/0` on every pre-threshold commit pool — so `simulation` /
`reverse_simulation` on such a pool aborted with a VM panic instead of a
decodable error. (Pre-C5, one direction already panicked via zero cw20
balances; C5 made both directions panic.) No fund risk — queries are
read-only — and the router path was already protected by its commit-phase
gate; the exposure was direct integrator queries.

**Fix:** explicit zero-reserve guard in both query functions returning
`"Pool has no active liquidity to quote against (pre-threshold or
drained)"`. Regression test:
`simulation_on_zero_reserves_errors_cleanly_instead_of_panicking`.

### DA-2 (Low, FIXED): `minimum_receive = 0` accepted while per-hop gates widened
C7 deliberately widens the per-hop spread gate from the pools' 0.5%
default to their 5% hard cap, making `minimum_receive` the **only**
end-to-end slippage protection — by design. But the router accepted
`minimum_receive: 0`, i.e. no protection at all. Pre-C7 such a caller's
worst-case sandwich extraction was bounded ≈ 0.5%/hop; post-C7 it became
≈ 5%/hop (≈14% over a 3-hop route). No honest flow wants a zero minimum
(frontends size it from `SimulateMultiHop`).

**Fix:** `RouterError::ZeroMinimumReceive` rejected at `start_multi_hop`
— the shared entry covering both the native and CW20 paths. Regression
test: `router_rejects_zero_minimum_receive` (asserts both paths).

## Per-change analysis

**C1 — CreatorEarnings.** Pure reads of already-public state
(`CREATOR_FEE_POT`, `CREATOR_EXCESS_POSITION`, threshold flags). All
loads are `may_load` with defaults except `COMMITFEEINFO`, which is
unconditionally written at creator-pool instantiate. `claimable_now` is
computed with the same comparison (`block.time >= unlock_time`) the
claim handler enforces — no drift between the query's promise and the
execute path's gate. No authorization needed (nothing secret), no state
written, gas O(1).

**C2 — Pools enumeration.** Limit clamped to ≤100; iterator is `range
... .take(limit)` so gas is bounded regardless of registry size;
`start_after = u64::MAX` yields an empty page; ordering is the Map's
ascending key order (deterministic). Returns only registry data the
per-address `PoolByAddress` already exposed.

**C3 — Marketing at instantiate.** Without this, cw20-base permanently
locks marketing (no admin can ever be set post-instantiate), so every
creator token would be un-brandable. The factory now passes
`marketing: Some({ marketing: <pool creator>, .. })`. Reviewed angles:
the *contract* admin (migration rights) is unchanged (still the
factory); the marketing admin only controls `UpdateMarketing` /
`UploadLogo` on the creator's own token; wire-compat with cw20-base ≥
0.13 confirmed (field exists; `logo: None` valid). Residual note:
marketing strings/logo URLs are **creator-controlled, untrusted
display data** — explorers must sanitize (the reference explorer
sanitizes text and should treat logo URLs as untrusted when rendering
is added).

**C4 — `last_committed` alias.** Deserialization-only addition;
canonical (typo'd) name unchanged, so no deployed client breaks.
`deny_unknown_fields` is unaffected by variant aliases. Round-trip
covered by `last_commited_query_accepts_both_spellings`.

**C5 — Accounting-reserve quoting.** The economic fix: balances include
non-AMM funds (LP fee reserves, creator fee pot, commit proceeds,
emergency escrow), so balance-based quotes overstated depth — measured
at ~6% optimistic on a freshly-crossed pool in deployment testing.
Quotes now use the same `POOL_STATE.reserve0/1` the execute path trades
against; `simulate == execute` is asserted end-to-end by the router's
`simulate_matches_execute`. Direction mapping (offer↔reserve0/1 by
`asset_infos` index) mirrors the previous matching logic exactly. See
DA-1 for the zero-reserve edge this surfaced.

**C6 — Registry-gated simulation.** Simulation previously sent the
commit-only `IsFullyCommited` to every hop (hard error on standard
pools). It now (1) resolves each hop via the factory's `PoolByAddress`
— the same authoritative check execution performs, now also rejecting
unregistered pools at quote time, strictly earlier and tighter than
before — and (2) runs the commit-phase check only for
`PoolKind::Commit`. Legacy registry records deserialize `pool_kind` as
`Commit` (serde default), which is the historically-correct
classification, so old pools keep the stricter check. Gas: ≤3 queries
per hop, hops ≤3. Read-only.

**C7 — Per-hop spread pinned to the hard cap.** A `None` max_spread is
*substituted* by pools with their 0.5% default — it does not disable
the gate — which silently failed every thin-pool route regardless of
the caller's `minimum_receive`, contradicting the router's documented
slippage model. Pinning `Some(5%)` (the widest value pools accept
without `allow_high_max_spread`) makes `minimum_receive` binding while
retaining the pools' own hard cap as defense-in-depth. The forwarded
value is asserted by `router_forwards_hard_cap_max_spread_per_hop`
via a mock-pool recorder. The widened unprotected worst case this
created is closed by DA-2.

**C8 — expand-economy instantiate guard.** Rejects the observed
deployment footgun (`factory_address` set to the deployer wallet as a
"placeholder"), which silently defers every threshold-crossing reward
and costs a 48h timelock to repair. The guard cannot reject a
legitimate flow: the factory never instantiates expand-economy, and no
wallet can be the factory contract. Deliberately narrow — a wrong
*contract* address still passes (caught post-deploy by
`scripts/verify_deploy.sh`, which cross-checks the wiring in both
directions).

## Invariants checked across the delta

- No new execute-path authority: the only new execute-side behaviors
  are two *rejections* (DA-2, C8). No privileged entry points added.
- No storage-format changes: no migrations required; all wire changes
  are additive (new query variants, new optional-shaped instantiate
  field, deserialization alias).
- All new query paths are gas-bounded (pagination clamps, hop caps) and
  panic-free (DA-1 closed the one violation).
- Fund-movement paths (commit, swap, liquidity, claims, distribution)
  are untouched by this delta except C7's spread-parameter change,
  whose end-to-end guard is strengthened by DA-2.

## Suggested focus areas for the external audit

1. **C5/C7 economics** — independent confirmation that
   `POOL_STATE`-based quoting matches execution under concurrent state
   changes within a block, and that the 5%-per-hop + mandatory
   `minimum_receive` model has no remaining MEV asymmetry worth
   tightening (e.g., whether the router should also expose an optional
   caller-supplied per-hop cap).
2. **C3 supply-chain** — pin and hash the exact cw20-base wasm the
   factory's `cw20_token_contract_id` will point at; this review
   verified wire-compat, not the deployed binary.
3. **The baseline itself** — this document covers only the delta;
   `SECURITY_AUDIT.md` plus this file together describe the full
   surface as of this branch.

## Test evidence

- Workspace: 663 tests / 0 failures (creator-pool 229, factory 254,
  standard-pool 76, expand-economy 40, pool-core 34, router 23, misc 7).
- Delta-specific: standard-pool hop simulate+execute, forwarded
  max_spread assertion, unregistered-pool rejection at quote time,
  reserve-sourcing regression (POOL_STATE diverged from balances),
  zero-reserve clean error (DA-1), zero-minimum rejection on both offer
  paths (DA-2), marketing-admin instantiate assertion, registry
  pagination (ordering / resume / end-of-data / clamp), both query-name
  spellings, expand-economy instantiator rejection.
- Release wasm builds compile for all five contracts;
  `ci/check_prod_build.py` passes.
