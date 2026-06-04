# Bluechip Protocol — Pre-Audit Security Review

**Reviewer:** Independent pre-audit review (CosmWasm / Cosmos SDK, AMM, oracle, multi-contract reply-chain).
**Purpose:** Investor-facing security-posture assessment + scope document for a paid third-party audit.
**Status of this document:** This is a *pre-audit* review intended to surface and prioritize risk. It is **not** a substitute for a full paid audit. Several subsystems (notably the ~2,900-line internal oracle and the Pyth wire-format types) were reviewed at the design/claim level and spot-checked against code, not line-by-line proven.

## Threat model used (agreed with the team)

- **Trusted (not adversarial):** factory admin, pool creators, keepers (oracle + distribution), Pyth publishers/VAA submitters. Admin centralization is an **accepted, documented assumption for the current testing phase**; the team intends to add multisig/governance before mainnet. Per that decision, pure "a fully-trusted admin can do X" findings are rated **Informational**, but the **most decision-relevant** ones are still called out with the mainnet-hardening step.
- **Adversary:** any **unprivileged external caller** — trader, LP, committer, arbitrary address, MEV bot, permissionless-keeper caller, and (for standard pools) a **hostile CW20 token contract**. Note: pool creation is **permissionless**, so "creator" in practice means *any* unprivileged user who paid the creation fee.
- **Key custody (as stated by the team):** one party currently holds **both** the CosmWasm code-admin/migrate key **and** the factory-admin key, as a single EOA. Consequence: every in-contract 48h timelock is bypassable via `MigrateContract` today (see I-1). This is the dominant investor caveat and is treated as Informational-by-decision but flagged prominently.

## Headline result

- **No Critical and no unprivileged-exploitable High severity in-contract vulnerability was found** by this review across the commit/threshold, swap, liquidity, oracle, expand-economy, factory-lifecycle, keeper, and creation paths.
- The codebase is **unusually defensive and audit-ready**: checked/`Uint256` arithmetic throughout, checks-effects-interactions ordering on every fund-moving path, multiple independent no-double-mint gates on the threshold crossing, named regression tests pinning prior fixes (`HIGH-2`, `HIGH-3`, `MEDIUM-1..4`), and extensive in-code rationale.
- The highest-priority items are **not in-contract logic bugs** but: (H-1) a **build/release-hygiene gap** that can ship a test-weakened wasm to mainnet; (I-1) **key/centralization custody**; and (M-1) **creator allocation economics**. These are the things to fix before the paid audit and before mainnet.

---

# 1. Factory — instantiation, admin authorization, timelocks, migration

**Attack surface / trust assumptions:** factory admin (trusted), the reply chain that fills the CW20 address, and `MigrateContract` (chain-admin key).

### Findings

**I-1 (Informational by decision; *highest investor caveat*) — All in-contract 48h timelocks are bypassable by the migrate key, which is a single EOA.**
- *Surface:* CosmWasm `MigrateContract` replaces the contract code wholesale, bypassing every in-contract guard. The factory (and pools, and expand-economy) are instantiated with an `admin` that today is one EOA, which also holds factory-admin.
- *Risk / rationale:* **Informational per the team's accepted testing-phase trust model**, but the *consequence if that one key is lost/compromised is Critical* — an attacker could migrate the factory and every pool to arbitrary code, drain reserves, and mint supply, with **no** 48h observation window. The 48h `ProposeConfigUpdate→UpdateConfig`, `UpgradePools`, `ForceRotateOraclePools` flows are all genuinely enforced in-contract (verified, see below) but are only as strong as the migrate key.
- *Exploit path:* compromise the single key → `MsgMigrateContract(factory, attacker_code)` → done in one tx, no delay.
- *Remediation (pre-mainnet, non-negotiable):* move the **code-admin/migrate key** to an n-of-m multisig or on-chain governance with its own delay, and the **factory-admin** to a separate multisig. Until then, every timelock in this report is advisory. Consider setting the contract `admin` to a governance/timelock contract rather than an EOA.

**L-1 — Migrate handlers do not validate the cw2 contract *name* (all four contracts).**
- *Surface:* `factory/src/migrate.rs:12-29`, `creator-pool/src/contract.rs:939-956`, `standard-pool/src/contract.rs:658-673`, `expand-economy/src/migrate.rs:36-50`.
- *Risk / rationale:* **Low.** Each handler correctly refuses a downgrade (`stored_semver > current ⇒ DowngradeRefused`) — *verified in all four* — but none assert `stored_version.contract == CONTRACT_NAME`. An operator (or governance) that fat-fingers a migrate, pointing one contract's address at a *different* contract's code whose version is `≤` target, would pass the guard and run the wrong migration logic against the wrong storage. Reachable only by the migrate key (trusted), so Low.
- *Remediation:* add `if stored_version.contract != CONTRACT_NAME { return Err(...) }` before the semver check in all four handlers. One line each.

**I-2 — `UpgradePools` accepts an arbitrary `new_code_id` (no on-chain allowlist).**
- *Surface:* `factory/src/execute/upgrades.rs` stores `new_code_id` and applies it via `WasmMsg::Migrate` with only the 48h timelock as the gate; no check that the code-id is an approved pool wasm.
- *Risk:* **Informational** (admin-trusted). Mainnet hardening: maintain an admin/governance-managed allowlist of approved pool code-ids and reject proposals outside it — this bounds the blast radius of a compromised admin to previously-reviewed code.

**I-3 — Three factory setters are timelock-exempt (but hard-capped).** `SetOracleUpdateBounty`, `SetDistributionBounty`, `SetPythConfThresholdBps` (`factory/src/execute/oracle.rs`) mutate immediately. Each is `ensure_admin`-gated and bounded by a hardcoded cap (`MAX_ORACLE_UPDATE_BOUNTY_USD=20_000`, `MAX_DISTRIBUTION_BOUNTY_USD=100_000`, conf bps clamped `[50,500]`). **Informational**; consider timelocking or emitting governance-visible events for `SetPythConfThresholdBps` since widening the Pyth confidence gate (200→500 bps) loosens price acceptance immediately.

**I-10 — `pool_create_cleanup` contract-address extraction has an events-fallback ambiguity.** The third-priority fallback scans events for the first `_contract_address`; if a future bundled wasm spawns child contracts in its own `instantiate`, the wrong address could be picked. Not exploitable with current wasms (none spawn children). **Informational**, flagged for the paid audit if the bundled wasms change.

### What the code substantiates (Factory)

| Claim | Verdict | Evidence |
|---|---|---|
| Every privileged ExecuteMsg variant is admin-gated; no unprivileged path reaches a privileged handler | **SUBSTANTIATES** | `ensure_admin` (`execute.rs:388-394`) on every config/upgrade/oracle/anchor/pool-admin handler; `Create`/`CreateStandardPool`/`UpdateOraclePrice` intentionally permissionless+fee/rate-gated; `NotifyThresholdCrossed` gated `sender==creator_pool_addr`; `PayDistributionBounty` gated to registered commit pools |
| 48h `ProposeConfigUpdate→UpdateConfig` enforced, no early-apply/replay; cancel sound | **SUBSTANTIATES** | `effective_after` check (`config.rs:200-203`), `PENDING_CONFIG.remove` post-apply (`:296`), re-propose blocked (`:329-333`); `ADMIN_TIMELOCK_SECONDS = 86_400*2` (`state.rs:182-186`) |
| `SetAnchorPool` is a true one-shot; `atom_denom` equality enforced | **SUBSTANTIATES** | `INITIAL_ANCHOR_SET` guard (`oracle.rs:272-279`), set true only after success (`:313`); `validate_anchor_pool_choice` requires exact `{bluechip,atom}` native pair (`:343-412`) |
| Reply chain (CW20-address fill) cannot be spoofed/re-entered; `POOL_CREATION_CONTEXT` keyed safely; `reply_on_success` ⇒ atomic cleanup | **SUBSTANTIATES** | reply IDs `(pool_id<<8)|step` minted by factory; `reply()` is runtime-only; all create steps `reply_on_success` ⇒ failure reverts whole tx |
| Code-ids come only from factory storage; sentinel blocks smuggling a hostile CW20 into the pair | **SUBSTANTIATES** | `CREATOR_TOKEN_SENTINEL` check (`create.rs:73-78`); code-ids from `FACTORYINSTANTIATEINFO` |
| Downgrade guard (`stored>current`) on all migrate handlers | **SUBSTANTIATES (name-check missing — L-1)** | all four handlers verified |
| `UpgradePools` excludes the anchor pool and can't brick the registry | **SUBSTANTIATES** | anchor exclusion at propose+apply (`upgrades.rs:99-114`, `196-212`); registry maps untouched by `Migrate` |

---

# 2. Internal Oracle — bluechip/USD derivation, manipulation resistance, staleness

**Attack surface:** unprivileged trader who can move anchor-pool reserves; Pyth value selection within the allowed age window; staleness/warm-up edges. (Privileged keepers/Pyth trusted, but their *timing discretion* is analyzed.)

### Findings

**M-2 — Post-reset / force-rotate TWAP drift dilution: a sustained anchor manipulation can seed a price wider than the nominal 30%/round.**
- *Surface:* `factory/src/internal_bluechip_price_oracle.rs` reset/warm-up state machine. After every reset (bootstrap-confirm, `SetAnchorPool`, timelocked anchor change, `ForceRotateOraclePools`) the TWAP window is cleared; for the first rounds the effective TWAP is a 1–2 observation average, so the `MAX_TWAP_DRIFT_BPS=3000` breaker is a *per-round-aggregate* 30% cap, not per-observation. A single admitted observation can be up to ~±60% of prior; two marginal rounds compound.
- *Risk / rationale:* **Medium.** Crucially, each "observation" is a **cumulative-delta TWAP read** (`:1771-1814`), *not* a spot price — so the attacker must **sustain** an anchor-reserve perturbation across the full `UPDATE_INTERVAL` (real capital, real time), then survive the 5-round warm-up before any *strict* consumer (commit valuation) is priced. So this is a capital-and-time-intensive bias, not a single-block steal — but the achievable post-warm-up bias exceeds the headline "30%."
- *Exploit path:* induce/await a reset → over rounds 1–2 push anchor reserves so the median-of-two seeds a biased `last_price` → keep biasing within 30%/round through warm-up → after warm-up, strict commit valuations price against the biased anchor (e.g., a $25k threshold crosses on a smaller true commit).
- *Remediation:* require a minimum observation **count** in-window before the branch-(a) TWAP goes live (so the first post-warm-up TWAP is diluted by real history), and/or tighten `MAX_TWAP_DRIFT_BPS` during warm-up, and/or widen `ANCHOR_CHANGE_WARMUP_OBSERVATIONS`. The team documents this trade-off in code; confirm the residual is within risk appetite.

**L-2 — Best-effort warm-up fallback can serve a pre-reset price into the standard-pool *creation fee* and *distribution keeper bounty*.**
- *Surface:* `usd_to_bluechip_best_effort` → `get_bluechip_usd_price_with_meta(allow_warmup_fallback=true)` returns `pre_reset_last_price` during warm-up (`:2257-2331`).
- *Risk / rationale:* **Low.** The **strict** commit-valuation path (`get_bluechip_usd_price`, `allow_warmup_fallback=false`) *hard-fails* during warm-up (`:2333-2338`, `:2450-2452`) — **verified that the security-relevant commit valuation cannot leak a stale price**. The leak is confined to the standard-pool creation fee and the keeper-bounty USD→bluechip conversion, both bounded (the fallback still layers live Pyth on top and re-validates cached Pyth conf, and `pre_reset` was itself armed under the breaker). Worst case is a small, breaker-bounded fee/bounty mispricing during a ~5-minute rotation window.
- *Remediation:* acceptable as designed; if undesired, price the standard-pool fee on the strict path (it already has a hardcoded bootstrap fallback).

**L-3 — Pyth `MIN_PYTH_AGE=10s` breaks same-block bundling but leaves a 10–300s value-selection window.**
- *Risk:* **Low** (documented residual). An actor can pre-push a favorable (≥10s old, <300s) signed ATOM/USD value and time a commit to consume it. Bounded by the 300s staleness cap + the Pyth confidence-bps gate. *Remediation:* none required if accepted; otherwise consume Pyth EMA for valuation or tighten staleness toward keeper cadence (trades off routine freezes).

### What the code substantiates (Oracle) — all 10 core claims hold

| Claim | Verdict | Evidence (`internal_bluechip_price_oracle.rs` unless noted) |
|---|---|---|
| bluechip/USD = anchor TWAP (bluechip/ATOM) × Pyth ATOM/USD; no inversion/decimal bug | **SUBSTANTIATES** | division form `atom_usd×PRICE_PRECISION/bluechip_per_atom_twap` (`:2428-2441`); pool pre-scales by `PRICE_ACCUMULATOR_SCALE=1e6` matching `PRICE_PRECISION` |
| 1h TWAP / 60s cadence; time-weighted, not spot; ~60 obs | **SUBSTANTIATES** | trapezoidal time-weighted mean (`:1869-1918`); cumulative-delta, `continue` on zero delta (`:1779-1814`) |
| `MAX_TWAP_DRIFT_BPS=3000` strict `>`, exactly-3000 accepted; first-update bypass not an arbitrary-seed; saturating fails closed | **SUBSTANTIATES (M-2 caveat)** | strict `>` (`:1421,1484`); bootstrap→branch(d) admin-confirmed (`:1566-1610`); overflow→`u128::MAX`→trip (`:307-317`) |
| 5-obs warm-up enforced; strict vs best-effort bifurcation; no stale leak into commit valuation | **SUBSTANTIATES** | warmup armed on bootstrap/rotate/anchor-change; strict hard-fails (`:2333-2338`); best-effort fallback only via `usd_to_bluechip_best_effort` |
| Pyth 300s staleness; u64 saturating-sub vs i64 wrap; 5s future skew; 10s min-age | **SUBSTANTIATES** | negative `publish_time` rejected pre-cast (`:2063-2068`); future-skew (`:2076-2081`); `saturating_sub`>300 (`:2086-2089`); age<10 (`:2111-2121`) |
| Cache bounded by `publish_time` not write-time (HIGH-2); can't exceed 300s true age | **SUBSTANTIATES** | cache stores `pyth_publish_time` (`:1313-1318`); fallback age check (`:2356-2362`) |
| Pool-side `MAX_ORACLE_STALENESS=120s`, accept at exactly `ts+120`, reject `+1s` | **SUBSTANTIATES** | `creator-pool/src/swap_helper.rs:85-92`; boundary test `audit_regression_tests.rs:3570` |
| Pyth outage + stale cache ⇒ strict callers fail **closed** | **SUBSTANTIATES** | `Err("Pyth price stale and no valid cached price")` (`:2344-2362`) |
| Basket-disabled path cannot leak into `last_price` | **SUBSTANTIATES** | `ORACLE_BASKET_ENABLED=false` ⇒ `select_random_pools_with_atom` returns `[anchor]` only (`:588-590`); `last_price=atom_pool_price` (`:1664-1672`) |
| Plain `mock` (vs `integration_short_timing`) doesn't weaken an oracle gate | **SUBSTANTIATES** | the 4 functional `cfg(mock)` sites are benign helpers / a fallback query; every gate-weakening is behind `integration_short_timing` — **but see H-1: `mock` itself wires the mockoracle price short-circuit, so a `mock`/`mock_only` wasm must never be a mainnet artifact** |

*Could not fully verify (for the paid audit):* the Pyth wire-format types (`pyth_types.rs`) are hand-mirrored from upstream and a schema bump fails deserialization at runtime — verify against the deployed Pyth version. The ~2,900-line TWAP/reset/breaker implementation was reviewed at the claim level + spot-checked, not line-by-line proven.

---

# 3. Standard Pool Creation Path

**Attack surface:** unprivileged caller creating a pool; fee handling; same-tx NFT-ownership accept.

### Findings

**M-4 (shared with §4) — Pre-anchor "bootstrap fallback" fee is Sybil-spammable during the launch window.** (See §4 for the full write-up; it applies to both pool kinds.) For standard pools the first creation *is legitimately* the anchor bootstrap, so the fallback is by-design there; the risk is the window before `INITIAL_ANCHOR_SET` flips.

### What the code substantiates (Standard creation)

| Claim | Verdict | Evidence (`create.rs`) |
|---|---|---|
| `must_pay` single-denom + non-zero fee enforcement | **SUBSTANTIATES** | `must_pay(&info, bluechip_denom)` (`:664`); multi/zero/wrong-denom revert ⇒ bank auto-refunds |
| Surplus over required fee refunded in same tx | **SUBSTANTIATES** | `surplus = paid - required`, BankMsg back to sender (`:711-731`) |
| Fee disabled ⇒ no funds accepted (no silent accept-then-refund) | **SUBSTANTIATES** | `:656-662` |
| Hardcoded fallback `100_000_000 ubluechip` only when `INITIAL_ANCHOR_SET==false` | **SUBSTANTIATES** | `:629-646`; after anchor set, oracle outage ⇒ refuse creation (`OracleUnavailable`) |
| Same-tx `AcceptNftOwnership` closes the pending-ownership window; factory-only; no front-run | **SUBSTANTIATES** | `standard-pool/src/contract.rs:547-582` factory-gated; cw_ownable `AcceptOwnership` requires `sender==pending_owner` (the pool) |
| Pair validation: no self-pair, canonical-bluechip required, CW20 leg must answer `TokenInfo` | **SUBSTANTIATES** | `validate_standard_pool_token_info` (`:426-498`) |
| Single-pool-per-pair uniqueness | **SUBSTANTIATES** | `PAIRS` guard in `register_pool` + pre-check (`:539-546`) |

*Non-finding:* the fee BankMsgs ride before the NFT-instantiate SubMsg in the Response; a sub-agent flagged possible reentrancy if `bluechip_wallet` were a contract. **Not exploitable** — Cosmos `BankMsg::Send` does not invoke contract code. Noted for completeness.

---

# 4. Creator Pool Creation Path

**Attack surface:** unprivileged "creator" (creation is permissionless); creator-set parameters; self-dealing via the threshold payout.

### Findings

**M-1 — Creator value-extraction / "soft-rug": the threshold payout mints a large *unlocked* creator allocation, and the threshold is self-fundable.**
- *Surface:* `creator-pool/src/commit/threshold_payout.rs` mints `creator_reward_amount = 325_000_000_000` (325k tokens, ≈ **38% of the 850k immediately-circulating supply**) **directly to the creator wallet with no vesting** (`mint_tokens(token,creator_wallet,325k)`); only the *excess* liquidity position is time-locked (`CREATOR_EXCESS_POSITION`). The 500k commit-return is distributed pro-rata by USD committed — so a creator who self-commits the full $25k threshold receives the entire 500k *and* the 325k = **825k of the ~850k circulating supply**.
- *Risk / rationale:* **Medium (economic/design).** Creators are nominally "trusted," but creation is **permissionless** (anyone can become a creator), and the victim class is **token buyers/committers**, who are not in a trust relationship. The only brakes on dumping are the **2-block post-threshold cooldown** + the **100-block swap-cap ramp** (0.5%→100% of offer reserve, ≈8 min on a 5s chain). After the ramp there is **no further restriction** — the creator can sell the unlocked allocation into the pool, extracting bluechip that organic buyers added. This is the classic launchpad-rug vector, and the protocol's mitigation (a short ramp, no vesting) is weaker than typical multi-day creator vesting.
- *Exploit path:* (1) create pool ($creation fee). (2) Optionally self-commit toward/through threshold to maximize the pro-rata share. (3) At crossing, receive 325k–825k unlocked tokens. (4) Wait out the 2-block cooldown + ~100-block ramp. (5) Sell the allocation into the pool as organic buyers add bluechip liquidity, extracting their capital and crashing the token.
- *Remediation:* add a **vesting/lockup** (linear over weeks/months) on `creator_reward_amount` (and consider on the self-committed share); or cap the creator's pro-rata commit-return share; or lengthen the post-threshold ramp. At minimum, **disclose the unlocked creator allocation prominently** to committers, since the economics are buyer-relevant. This is squarely the "can a creator extract value via the threshold payout structure?" question — answer: **yes, post-ramp, by design**, and it should be a deliberate economic decision.

**M-4 — Bootstrap fallback fee applies to creator-pool creation during the pre-anchor window (cheap, Sybil-spammable pool creation).**
- *Surface:* `execute_create_creator_pool` (`create.rs:222-252`) uses the same fallback as standard pools: if the oracle is unavailable **and** `INITIAL_ANCHOR_SET==false`, it charges the flat `STANDARD_POOL_CREATION_FEE_FALLBACK_BLUECHIP = 100 bluechip` (≈ $0.10 at launch parity) instead of the configured USD fee.
- *Risk / rationale:* **Medium (leaning Low).** Bounded to the pre-anchor launch window (the admin sets the anchor early, flipping `INITIAL_ANCHOR_SET`), and pools created then are **non-functional until the anchor exists** (commit valuation needs the oracle). But during that window an attacker can create pools at a ~100–500× discount vs. the configured USD fee, throttled only by the 1h per-address rate limit (Sybil-circumventable with many addresses) — registry/`pool_id` pollution and cheap pool farming.
- *Remediation:* gate creator-pool creation on `INITIAL_ANCHOR_SET==true` (creator pools should not be creatable before the anchor exists); restrict the flat fallback to the *first* standard-pool creation only.

**I-8 — README says creator Create is "permissioned"; the code is permissionless (fee + 1h rate-limit only).** No admin allowlist gates `execute_create_creator_pool`. Not a vulnerability, but a **documentation/expectation mismatch** investors should know about (it widens "creator" to "anyone").

### What the code substantiates (Creator creation)

| Claim | Verdict | Evidence |
|---|---|---|
| 1h per-address rate limit (per-caller, not global) | **SUBSTANTIATES** | `LAST_COMMIT_POOL_CREATE_AT: Map<Addr,_>` (`create.rs:174-199`), `COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS=3600` |
| USD creation fee as anti-spam; `must_pay`; surplus refund | **SUBSTANTIATES** | `create.rs:280-321` |
| Factory mints the CW20 (sentinel rewrite); 6-decimal enforced | **SUBSTANTIATES** | sentinel (`:73-78`); `decimals!=6 ⇒ reject` (`:95-99`) |
| Mint cap pinned to the threshold-payout total; over-mint fail-closed | **SUBSTANTIATES** — **but the value is 1.2M, not the README's "1.5M" (I-8)** | `cap: Some(threshold_payout_amounts.total_mint())` = 325k+25k+350k+500k = **1.2M** tokens (`create.rs:363`); cw20-base rejects any mint beyond cap |
| Payout splits validated at instantiate **and** runtime | **SUBSTANTIATES** | `validate_pool_threshold_payments` + `trigger_threshold_payout` both check exact constants (`threshold_payout.rs:45-93,214-222`) |
| Creator cannot set adversarial threshold/fee parameters | **SUBSTANTIATES** | `commit_threshold_limit_usd`, commit fees, payout splits all come from **factory** config, not creator input (`pool_creation_reply.rs:142-147`) — the residual creator lever is the *unlocked allocation* (M-1), not parameter-setting |

---

# 5. Commit Logic (including threshold crossing) — **highest-stakes, verified in depth**

**Attack surface:** unprivileged committer at the threshold boundary; multi-tx sequences; the distribution state machine.

### Result: the core conservation invariants hold.

- **No double-cross / no double-mint — multiple independent gates, all verified:**
  1. Dispatcher routes to the crossing handlers only when `IS_THRESHOLD_HIT==false` (`commit.rs:279`).
  2. Each crossing handler re-checks `IS_THRESHOLD_HIT` at entry (`threshold_crossing.rs:80-82,381-383`).
  3. `trigger_threshold_payout` is the **load-bearing** gate: checks `IS_THRESHOLD_HIT==false` at entry, sets it `true` only after mint+seed completes (`threshold_payout.rs:152-153,343`).
  4. `THRESHOLD_PROCESSING` flag + the contract-wide `REENTRANCY_LOCK` wrap the whole commit (`commit.rs:113-124,307-313`).
  Any single future call site that bypassed the handlers still cannot re-mint.
- **Cannot distribute more than minted (verified):** sum of `COMMIT_LEDGER` equals exactly `commit_amount_for_threshold_usd` (pre-threshold commits add their USD; the crosser's ledger entry is credited only `usd_to_threshold`, capping the sum). `DistributionState.total_committed_usd` is set to that same threshold value, so per-user reward `= usd_paid × 500k / total_committed_usd` sums to ≤ 500k, with the floor-division residual (≤ N−1 base units) **minted to the creator** on the final batch (`distribution_batch.rs:201-238`). The CW20 cap (1.2M) = seed 350k + creator 325k + protocol 25k + distribution 500k **exactly** — zero headroom, so any over-mint fails closed at cw20-base.
- **Boundary handling verified:** exact-hit vs excess-split branches are arithmetically consistent; `NATIVE_RAISED_FROM_COMMIT` stores **net-of-fees** so `pools_bluechip_seed` is exact (no recovery-floor dust); the 3%-of-reserve excess-swap cap + 5% default slippage + dust-guard + refund are all present (`threshold_crossing.rs:182-277`).
- **Crossing-time captured once:** oracle rate snapshotted at handler entry and threaded through (`commit.rs:188-191`); `THRESHOLD_CROSSED_AT` snapshotted so a retried factory-notify mints the amount owed at *original* crossing time (MEDIUM-2).
- **Post-threshold MEV controls:** 2-block cooldown + 100-block swap-cap ramp gate followers; the crosser's own bounded excess swap runs before any other tx observes the cooldown item (`state.rs:506-625`, `threshold_crossing.rs:125-138`).
- **Distribution state machine:** batches of ≤40, per-mint `reply_always` isolation into `FAILED_MINTS` (claimable later), stall timeout (24h) with admin (1h) and **permissionless** (7d) recovery, bounty paid only when `processed_count>0`. No path found that crosses twice or distributes more than minted. Two keepers in the same block are serialized by the chain and observe the advanced cursor — no double-processing.

### Findings

- **13s per-wallet commit rate limit, 6% fee split:** verified (`commit.rs:240-266`, `DEFAULT_SWAP_RATE_LIMIT_SECS=13`). Fee split is `commit_fee_bluechip + commit_fee_creator` with `total_fees < amount` and `amount_after_fees != 0` guards. **SUBSTANTIATES.**
- **State desync between drain and recovery:** the recovery paths cannot both persist a "give up" state and return `Err` (CosmWasm reverts writes on `Err`) — the code correctly surfaces typed errors and relies on the cursor + recovery windows, and documents why the dead `save+Err` patterns were removed (`distribution_batch.rs:259-310`). No desync that over-distributes was found.
- **I-8 (display):** `NATIVE_RAISED_FROM_COMMIT` / "raised toward goal" reports **net-of-fees**, so a $25k-threshold pool shows ~$23.5k of bluechip "raised" at crossing while per-user `total_paid_*` shows gross. By design and documented; a UX/reporting nuance, not a bug.

---

# 6. Expand Economy Contract

**Attack surface:** unprivileged caller vs. the factory-only `RequestExpansion`; the decay mint formula; the 24h cap.

### Result: access control and arithmetic are sound; only Informational findings.

- **`RequestExpansion` is factory-only** (`require_factory_caller`, exact `Addr` equality) **with bluechip-denom cross-validation** against the factory's `Factory{}` response, and the mint denom is always `config.bluechip_denom` (never caller-supplied). **SUBSTANTIATES.** Owner vs. factory roles are cleanly separated; `nonpayable` runs before dispatch on every handler.
- **Decay formula `500 − ((5x²+x)/((s/6)+333x))`** lives in `factory/src/mint_bluechips_pool_creation.rs` (not in expand-economy). Verified: overflow-guarded (`MAX_DECAY_X=1e9` early-return + `checked_mul`), division-by-zero handled (`s=0,x=0 ⇒ denominator 0 ⇒ return 500_000_000`), floor-at-zero (subtraction only when `division_result < base`), and **`x` is not attacker-inflatable** — it is `commit_pool_ordinal`, allocated **only at a real threshold crossing** behind the `POOL_THRESHOLD_MINTED` one-shot gate (standard pools and junk creates don't inflate it), and `crossed_at` is clamped to `≤ block.time`. **SUBSTANTIATES.**
- **24h `DAILY_EXPANSION_CAP` is a true sliding window** (entries older than 24h pruned per call), rejects (does not clamp) over-cap requests, and **skipped/failed requests don't burn budget** (persist happens only on the mint path). **SUBSTANTIATES.**
- **48h config/withdrawal timelocks** enforced (no early-apply/replay; owner rotation auto-clears a pending withdrawal). **Denom regex** is a hand-rolled, ReDoS-free validator matching the cosmos-sdk rule. **SUBSTANTIATES.**

### Findings

- **I-6 — Daily-cap exhaustion can *delay* (not lose) late-pool expansions** if many pools cross in one day: a late `RequestExpansion` reverts with `DailyExpansionCapExceeded`, reverting the whole `NotifyThresholdCrossed` (atomic), and the pool retries via `RetryFactoryNotify` next window. Not unprivileged-triggerable (factory-only caller). **Informational**; document for operators; consider per-pool budgeting or partial-fulfillment.
- **I-7 — `EXPANSION_LOG` (Vec) is rewritten per call**; worst case (~1e5 tail mints/day) is a large read/write. Bounded by the cap; **Informational**; consider a minimum-mint floor.
- **L-1 (shared) — migrate handler lacks the contract-name check** (same as Factory L-1).

---

# 7. Swap Logic — constant-product, slippage, fees, reentrancy

**Attack surface:** unprivileged trader; hostile CW20 on standard pools; the shared reentrancy lock.

### Result: standard Astroport-class math, correctly guarded.

- **Constant-product (`compute_swap`)** uses `Uint256` intermediates; the commission is redirected to the LP fee-reserve (not retained in the swap reserve), so the swap reserves preserve `x·y=k` net of the fee, with the fee accruing to LPs — the canonical model. Overflow-guarded; zero-offer ⇒ zero; **dust-swap rejection** (`return_amt==0 ⇒ ZeroAmount`) on every swap/commit path prevents "absorb offer, return nothing." `MINIMUM_LIQUIDITY` pre- and post-state floors on both sides. **SUBSTANTIATES.**
- **Slippage:** default `0.5%`, **hard cap `5%` (or `10%` with `allow_high_max_spread`)** — see I-8: this is **stricter** than the README's "max-50%." Belief-price and no-belief-price branches both implemented; zero belief price rejected.

### Findings

- **L-4 — The `REENTRANCY_LOCK` is a *within-execution-frame* guard only; the real protection is checks-effects-interactions + the standard-pool balance-verify.** `with_reentrancy_guard` (`packages/pool-core/src/generic.rs:34-48`) sets the lock at entry and **clears it when the handler returns — before** the dispatched `CosmosMsg`/`SubMsg` execute. In CosmWasm a hostile CW20's `Transfer`/`TransferFrom` hook runs *after* the parent handler committed and cleared the lock, so a cross-message callback finds `lock=false`. **Why this is not currently exploitable:** every fund-moving path satisfies CEI — all reserve/position/fee writes are saved **inside** the guarded body before the Response (carrying the external messages) is returned (verified in swap, commit, deposit, add, remove, collect). For standard pools (the only place arbitrary CW20 code runs) the `DEPOSIT_VERIFY_REPLY_ID` strict `post±outgoing == pre+credited` reply (`packages/pool-core/src/balance_verify.rs`) would catch any balance manipulation by a re-entrant hook and revert. *Severity: Low* (no concrete exploit), but the README framing that "the lock covers every hot path and can't be bypassed by a hostile CW20 hook" **overstates the mechanism** — the lock would not stop a cross-message re-entry; CEI + balance-verify do. *Remediation:* keep CEI invariant load-bearing; document the lock's true scope; have the paid audit fuzz hostile-CW20 re-entry on standard pools specifically (the `swap.rs` CW20 receive-hook balance pre-check at `:352-379` and the deposit/add verify reply are the critical surfaces).
- **I-9 — Sub-unit rounding favors the trader, and sub-334-unit outputs pay zero LP fee.** `compute_swap`'s gross return is `ask − floor(k/(offer+Δ))`, i.e., rounded **up** by `<1` base unit (trader-favorable, *opposite* the README's implied "always in the pool's favor"); commission floors to 0 for gross returns `<334` at 0.3%. **Informational** — sub-unit, gas-bounded, not profitably amplifiable (dust guard + `MINIMUM_LIQUIDITY` + gas cost ≫ 1 base unit of a 6-dp token).

### What the code substantiates (Swap)

| Claim | Verdict |
|---|---|
| Constant-product preserved; commission to LP fee-reserve; `Uint256` intermediates | **SUBSTANTIATES** |
| Default 0.5% / **max 5–10%** slippage (README "50%" is wrong; code is *safer*) | **SUBSTANTIATES (doc mismatch I-8)** |
| Zero-amount-swap rejection; dust-return rejection on all paths | **SUBSTANTIATES** |
| 6-decimal creator-token enforced (ties to hardcoded payouts) | **SUBSTANTIATES** (`create.rs:95-99`) |
| Reentrancy lock covers every hot-path call site | **PARTIALLY** — covers call sites, but protection is CEI + balance-verify, not the lock (L-4) |
| Post-threshold ramp bounds per-tx MEV | **SUBSTANTIATES** (`state.rs:557-625`) |

---

# 8. Liquidity Logic — NFT positions, first-depositor lock, fee-growth, emergency

**Attack surface:** unprivileged LP; first-depositor/donation/rounding attacks; pool_state vs. bank-balance divergence.

### Result: Uniswap-V3-style accounting, conservative and correct.

- **First-depositor `MINIMUM_LIQUIDITY=1000` lock is genuinely enforced** (not cosmetic): the first position carries `locked_liquidity=1000` that **can never be withdrawn** (remove paths subtract the locked slice) yet still accrues fees — defeating the classic share-inflation attack (`deposit.rs:385-392`, `remove.rs:71-77,238-245`). The threshold-seed liquidity is **virtual/unowned** (`total_liquidity=sqrt(r0·r1)`, no NFT minted), so post-threshold first LPs can't inflate either.
- **Fee-reserve solvency:** LP payout and creator-clip are each **capped at the remaining `fee_reserve`** (`liquidity_helpers.rs:164-198`, `remove.rs:305-358`), so claims can never exceed what's actually in the reserve; the unowned seed's fee share simply stays in the reserve (conservative). The MEDIUM-3 preserved-clip routing is fixed and tested.
- **Strict per-asset fund collection / orphaned-coin rejection:** `prepare_deposit` rejects any attached denom that isn't a pool side (`deposit.rs:160-181`); native overpayment refunded; CW20 pulled exactly via `TransferFrom`.
- **Standard-pool FoT/rebase defense:** `reply_on_success` balance-verify enforces **strict** `post±outgoing == pre+credited` (`balance_verify.rs`), so fee-on-transfer / rebasing CW20s revert deposits — pool_state can't drift from bank balance. Creator pools (trusted cw20-base) skip the verify.
- **Auto-pause below `MINIMUM_LIQUIDITY`** arms on remove paths and keeps **deposits open** (recovery) while rejecting swaps/removes; hard/emergency pauses override. Two-phase emergency withdraw (24h timelock) drains to `bluechip_wallet_address`, with a 1-year per-position pro-rata claim window before the admin may sweep residual. **SUBSTANTIATES.**

### Findings

- **L-6 — Creator-pool small-LP fee clipping transfers up to 90% of a small position's fees to the creator pot.** `calculate_fee_size_multiplier` scales fees from 10%→100% over `[0, OPTIMAL_LIQUIDITY=1e6]`; the clipped slice flows to `CREATOR_FEE_POT` (creator-claimable) on creator pools (`liquidity_helpers.rs:331-338`, `fees.rs`). Documented as a dust-griefing deterrent, but it's an **aggressive value transfer from small LPs to the creator** that small LPs may not expect. Standard pools bypass it (use a `MIN_STANDARD_POOL_POSITION_LIQUIDITY` floor instead). **Low/Informational** — disclose to LPs.
- *Checked, no finding:* first-depositor, donation, and rounding-dust attacks — the lock + virtual seed + fee-reserve caps + strict fund collection close them. No divergence between `pool_state` and bank balances found on the standard-pool path (balance-verify) or creator-pool path (trusted token, exact native).

---

# 9. Keeper Logic — bounties, liveness, admin-drain math

**Attack surface:** permissionless `UpdateOraclePrice` and `ContinueDistribution`; keeper timing; off-chain keeper liveness.

### Findings

- **L-5 — The oracle-update bounty cooldown is GLOBAL, so one keeper can monopolize all bounties.** `UpdateOraclePrice` gates on a single shared `last_update + UPDATE_INTERVAL(60s)` (`internal_bluechip_price_oracle.rs:1064-1076`). The first caller each 60s window wins; others get `UpdateTooSoon`. **Low** — a fairness issue, not a security one; protocol liveness needs only *one* working keeper.
- **I-5 — Distribution bounty is per-caller (5s), Sybil-spammable across addresses but bounded.** `LAST_CONTINUE_DISTRIBUTION_AT: Map<Addr,_>` (`distribution.rs:51-64`). Different addresses can call concurrently, but the bounty pays only when `processed_count>0` and total work is bounded by the committer count — Sybil spam *accelerates* distribution (good for committers) and merely drains the factory bounty reserve faster. **Informational.**
- **Admin-compromise drain math verified (~$10.5k/yr):** `MAX_ORACLE_UPDATE_BOUNTY_USD=20_000` ($0.02) × `86400/60` calls/day = $28.80/day ≈ **$10.5k/yr**, cap enforced strictly at `SetOracleUpdateBounty` (`oracle.rs:75-88`). The product (per-call cap × calls/day) is the documented admin-compromise budget. **SUBSTANTIATES.**
- **Keeper timing discretion:** because each observation is a cumulative-delta TWAP read (not a spot at call time), a keeper choosing *when* in the window to call changes which snapshot pair is captured, not the TWAP itself — the 30% breaker + sustained-capital requirement bound any influence (ties to M-2). A malicious keeper cannot single-handedly bias commit valuation or threshold crossing beyond M-2's bounds. **Informational.**
- **Liveness dependency:** if oracle-update keepers stall, the pool-side 120s staleness gate freezes commits (fail-closed) until refresh; if distribution keepers stall, the 24h stall timeout + admin (1h) and permissionless (7d) recovery prevent a permanent brick. Acceptable, but **production keeper liveness must be redundant + monitored** — verify the off-chain `keepers/` bots and Pyth VAA submission have HA before mainnet.

---

# Cross-cutting summary

## H-1 — **Build/release hygiene: the documented build path ships a test-weakened wasm. (HIGH — fix before the paid audit / mainnet.)**

- *Surface:* `Makefile` `build` and `optimize-factory` targets, `factory/Cargo.toml` `[[package.metadata.optimizer.builds]]`.
- *Finding:* The only two declared optimizer builds are **`mock`** (`mock` + `integration_short_timing`) and **`mock_only`** (`mock`). **There is no empty-feature production optimizer build.** Worse, `make optimize-factory` **renames `factory-mock.wasm` → the canonical `artifacts/factory.wasm`** (Makefile:62-71), so the "default" artifact is the **never-ship** build (120s timelocks, warm-up cleared every call, `UpdateTooSoon` bypassed, liquidity floors → 1000, **basket oracle ON**). And the `mock` feature itself wires the **mockoracle price short-circuit**, so even `mock_only` reads a *mock* price, not Pyth. CI (`ci.yml:66`) compiles the prod-feature build (proving it compiles) but does **not** produce or gate the deployable artifact. There is **no automated guard** that fails if a `mock`/`integration_short_timing` wasm is about to be deployed.
- *Why HIGH:* the README is explicit ("NEVER ship"), and a deployer following the Makefile would ship a contract whose oracle returns mock prices and whose timelocks are 120s — an **unprivileged attacker could then manipulate threshold crossings and speed-run governance**. The *likelihood* depends on release discipline; the *consequence if realized* is Critical. This is the single most actionable pre-mainnet item alongside key custody.
- *Remediation:* (1) add an **empty-feature** optimizer build and make it the canonical `factory.wasm`; (2) add a **CI/release gate** that fails if the deployable wasm was built with any of `mock`/`integration_short_timing`/`testing` (e.g., a dedicated `cargo build -p factory` with `--no-default-features` + an artifact-provenance check, and/or an instantiate-time `build_profile` attribute the deploy script asserts is `prod`); (3) consider a runtime guard that refuses to instantiate a `mock` build on a mainnet chain-id; (4) verify the wasms already committed to the repo root (`pool_optimized.wasm`, etc.) and any testnet artifacts are not mock builds before reuse.
- *Remediation status (addressed on this branch):* steps (1) and (2) are implemented. `factory/Cargo.toml` and `expand-economy/Cargo.toml` now declare a `prod` optimizer build with `features = []`; the Makefile copies the `-prod` artifact onto the canonical `<crate>.wasm` and **hard-fails** if the `-prod` artifact is missing (so the canonical name can never silently be a mock build); the `build` target emits the mock factory under `factory-mock.wasm`; and a new `prod-artifact-guard` CI job (`ci/check_prod_build.py` + a `--no-default-features` compile) fails the build if the `prod` optimizer build ever gains `mock`/`integration_short_timing`. Steps (3) (runtime mainnet-chain-id guard) and (4) (auditing the already-committed `*.wasm` blobs) remain open and are recommended before mainnet.

## Severity-ranked findings table

| ID | Sev | Area | Finding |
|---|---|---|---|
| **H-1** | **High** | Build/Release | Documented build path ships a `mock`+`integration_short_timing` wasm under the canonical artifact name; no CI guard. Consequence-if-shipped: Critical. |
| **M-1** | **Medium** | Creator/Commit | 325k–825k **unlocked** creator allocation at threshold (no vesting); self-fundable threshold; only a 2-block cooldown + ~8-min ramp brake → buyer-facing soft-rug vector. |
| **M-2** | **Medium** | Oracle | Post-reset/force-rotate TWAP drift dilution: sustained anchor manipulation can seed a bias beyond the nominal 30%/round; warm-up bounds, doesn't eliminate. |
| **M-3** | **Medium** | Router/Swap | Router never validates hop `pool_addr` against the factory registry; `minimum_receive` is the only protection (stored `factory_addr` is unused dead code). |
| **M-4** | **Medium** | Creation | Pre-anchor flat fallback fee applies to creator (and standard) pool creation → Sybil-cheap pool creation during the launch window. |
| **L-1** | Low | All migrate | Migrate handlers don't check the cw2 contract **name** (downgrade-by-version is checked). Wrong-wasm migration risk. |
| **L-2** | Low | Oracle | Best-effort warm-up fallback leaks a pre-reset price into the standard-pool fee + keeper-bounty conversion (not the strict commit path). |
| **L-3** | Low | Oracle | Pyth 10–300s value-selection MEV window (documented residual). |
| **L-4** | Low | Swap/Liquidity | Reentrancy lock is within-frame only; protection is CEI + balance-verify; README overstates the lock. |
| **L-5** | Low | Keeper | Global oracle-bounty cooldown → single-keeper monopoly (fairness). |
| **L-6** | Low | Liquidity | Creator-pool small-LP fee clipping routes up to 90% of small-LP fees to the creator pot. |
| I-1 | Info | Custody | Single EOA holds code-admin/migrate **and** factory-admin → all in-contract timelocks bypassable today. *Top investor caveat.* |
| I-2 | Info | Factory | `UpgradePools` has no code-id allowlist. |
| I-3 | Info | Factory | 3 oracle setters timelock-exempt (hard-capped). |
| I-4 | Info | Router | No migrate entry-point → router immutable after deploy. |
| I-5 | Info | Keeper | Distribution bounty Sybil-spam bounded by committer count. |
| I-6 | Info | Expand | Daily-cap exhaustion can delay (not lose) late-pool expansions. |
| I-7 | Info | Expand | `EXPANSION_LOG` Vec rewrite grows at tail-mint scale. |
| I-8 | Info | Docs | README mismatches: mint cap is **1.2M not 1.5M**; max slippage **5–10% not 50%**; creator Create is **permissionless not permissioned**; "raised" displays net-of-fees. |
| I-9 | Info | Swap | Sub-unit rounding favors trader; sub-334-unit outputs pay 0 fee. |
| I-10 | Info | Factory | `pool_create_cleanup` events-fallback multi-child ambiguity (future risk). |

## Top risks an investor should know about

1. **Release/build control (H-1).** The protocol's biggest *realizable* risk today is not a code bug but shipping the wrong binary. Fix the build path + add a CI gate before anything else.
2. **Key custody & centralization (I-1).** One EOA can migrate every contract past all timelocks. Until this is a multisig/governance with its own delay, the elaborate 48h timelock machinery is advisory. This is the #1 thing to harden for a credible mainnet story.
3. **Creator allocation economics (M-1).** A large unlocked creator allocation + self-fundable threshold + only an ~8-minute dump-ramp is a buyer-facing soft-rug vector. This is an economic-design decision (add vesting?) more than a bug, but it's the thing token buyers are most exposed to.
4. **Oracle is the valuation backbone, and its reset windows + Pyth optionality (M-2, L-2, L-3) + keeper liveness (L-5) are the residual surface.** Manipulation resistance is genuinely strong (time-weighted cumulative-delta TWAP, 30% breaker fail-closed, fail-closed staleness), but the reset/warm-up window and Pyth dependency deserve the deepest paid-audit attention.
5. **Router trusts caller-supplied pool addresses (M-3).** Hardened by `minimum_receive`, but a registry check is cheap defense-in-depth against malicious-frontend routing.

## Audit-readiness assessment

**High for the code; the gating work is operational/economic.** This is materially more audit-ready than typical pre-audit CosmWasm: pervasive checked/`Uint256` math, CEI ordering everywhere, multiple independent invariants on the highest-stakes flow (threshold crossing), fail-closed oracle posture, and — unusually — **named regression tests pinning each prior fix** plus a written invariant inventory (`FUZZ_REVIEW.md`, 22 invariants) and constants-rationale doc (`docs/ORACLE_CONSTANTS.md`). The paid audit can spend its budget on *depth* rather than *orientation*. The blockers to "audit-ready" in the strict sense are: H-1 (build hygiene), I-1 (custody), and a decision on M-1 (economics).

## Recommended scope/focus for the paid audit

1. **Internal oracle (deepest):** line-by-line proof of the TWAP/reset/warm-up/breaker state machine and the bootstrap-confirm path (M-2); the Pyth wire-format types vs. the deployed Pyth version; fuzz the reset windows and Pyth value selection. ~2,900 lines none of this review proved line-by-line.
2. **Commit/threshold/distribution conservation under adversarial multi-tx fuzzing:** extend the existing stateful proptest harness with the 22 invariants in `FUZZ_REVIEW.md`, especially "minted ≤ scheduled payout," "ledger sum == threshold," "no cross twice," and "distribution drains the ledger exactly."
3. **Hostile-CW20 reentrancy on standard pools** + the balance-verify reply equality (L-4): prove CEI holds on every path and the verify catches every manipulation.
4. **Economic review** of the threshold payout / creator allocation (M-1) and the decay-mint curve.
5. **Router** registry validation + multi-hop edge cases (M-3).
6. **Release engineering / deployment** (H-1) and **key-management/governance** (I-1) as explicit non-code scope items.

## Explicit note on the "must never ship" feature flags

Per the README's warning, the `mock` and `integration_short_timing` flags **must never reach a production artifact** — and **this review found a concrete path by which they can (H-1):** the canonical `artifacts/factory.wasm` produced by the documented Makefile targets is the `mock`+`integration_short_timing` build, there is no empty-feature optimizer build defined, and no CI/release gate prevents deploying a flagged wasm. `integration_short_timing` collapses timelocks to 120s, clears warm-up every call, bypasses `UpdateTooSoon`, lowers liquidity floors to 1000, and turns the basket oracle on; `mock` additionally wires the mockoracle price short-circuit. Treat closing this path (a real production build + a hard CI gate + ideally a runtime mainnet-chain-id guard) as a release-blocking item.
