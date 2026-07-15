# Security Audit — bluechip-osmosis-contract

**Scope:** `factory`, `creator-pool`, `router`, `packages/pool-core`, `packages/pool-factory-interfaces`
**Target chain:** Osmosis (CosmWasm 2.x, osmosis-std 0.27, x/gamm + x/tokenfactory + x/poolmanager + x/twap)
**Commit / branch:** `claude/bluechip-osmosis-audit-1iom52`
**Reviewer posture:** Tier-1 (Trail of Bits / OtterSec) rigor; findings traced to specific lines.

---

## Executive Summary

The migration off the internal AMM/CW20 onto Osmosis-native modules is, on the whole, **carefully engineered and defensively coded.** Authorization is complete and correct on every privileged entry point, the 48h config timelock cannot be bypassed or param-swapped, the pool→factory `NotifyThresholdCrossed` callback is bound to the registered pool address with an idempotency gate, reentrancy is guarded, arithmetic is `checked_*` throughout with `overflow-checks = true` in release, and there are no reachable `unwrap()/expect()/panic!` in production code. The threshold-crossing "FIX-E/FIX-C" seed/reserve accounting is internally consistent — I verified the balance invariant `pool_osmo_balance == pools_bluechip_seed + reserved` holds through the crossing, refund, seed, and remit sequence, so the pool never bricks the create on the *balance* side and the creator's earmark is always fully backed.

**The single most important issue is not an attacker exploit — it is a latent bricking condition at the most fragile moment in the whole lifecycle: the threshold crossing.** The native GAMM pool is created with `SubMsg::reply_on_success(MsgCreateBalancerPool)`. The `x/gamm` module charges the chain's `PoolCreationFee` (1000 OSMO on Osmosis mainnet, governance-adjustable) *on top of* the seeded liquidity. The contract funds this from a reserve that is (a) defaulted to **zero**, (b) **never validated** against the live chain param, and (c) filled only from 1% of commits. If the configured `gamm_pool_creation_fee` is less than the live chain fee — which is the default, and which a single governance vote can cause post-deployment — the create reverts, which reverts the entire crossing tx, and the pool becomes **permanently stuck pre-threshold.** Because pre-threshold pools have **no refund or withdrawal path** (emergency withdraw is explicitly disabled pre-threshold and no cancel/refund handler exists), every committer's funds in that pool are permanently locked. This is the top item to resolve before board submission (H-01, H-02).

Beyond that, the material items are: the creator token's on-chain denom metadata (name/symbol/decimals) is **never registered** — the creator's chosen name is effectively discarded except for deriving the subdenom, so explorers/wallets show a raw `factory/{addr}/{sub}` denom (M-01); the "estimate-derived" slippage floor does not actually protect against sandwiching for users who don't pass a `belief_price` (M-02); the router sweeps its whole balance per hop, making stray funds claimable (M-03); and the factory `migrate()` lacks a cw2 contract-name check and does an unbounded registry back-fill that can eventually make the factory un-upgradeable (M-04, M-05).

**Recommendation:** Do not deploy to mainnet until H-01/H-02 are resolved (validate/provision the GAMM creation fee and add a pre-threshold exit path), and M-01 is resolved (set denom metadata) since it directly defeats one of the product's stated requirements. The remaining Medium/Low items are hardening.

---

## Answers to your functional questions (behavioral verification)

| Your question | Verified answer |
|---|---|
| Still one factory creating all the GMMs? | **Yes.** Single `factory` contract, monotonic `POOL_COUNTER`, `register_pool` writes the registry. Each pool is its own contract instance that creates one native GAMM pool at crossing. |
| GMMs only become active after the 25k threshold? | **Yes.** The native GAMM pool is *created* only inside `trigger_threshold_payout` at crossing. Before that, `POOL_ID` is unset and `SimpleSwap` rejects with `ShortOfThreshold`. Threshold is USD-valued via x/twap (default $25k, configurable). |
| Do creators still get to name their token? | **Partially — see M-01.** `name`/`symbol`/`decimals` are validated, but only the lowercased `symbol` is used (to derive the subdenom). No `MsgSetDenomMetadata` is ever sent, so the name/symbol/decimals are **not** registered on-chain. |
| Minting event on crossing to creator / committers / bluechip / pool? | **Yes.** `trigger_threshold_payout` mints the four canonical splits: creator 325B, bluechip 25B, pool-seed 350B (to the pool for AMM seeding), commit-return 500B (funds the committer airdrop). All via TokenFactory `MsgMint`. |
| Cap on how far above threshold the crossing tx can go? | **Yes.** The crossing commit counts only `value_to_threshold` toward the raise; the entire post-fee **excess is refunded** to the crosser (`threshold_crossing.rs:102-112`). You cannot over-shoot the recorded raise. |
| Gate holding excess OSMO up to the max, eventually to creator? | **Yes.** When net raised > `max_bluechip_lock_per_pool`, the excess OSMO + proportional creator tokens are time-locked in `CREATOR_EXCESS_POSITION` and claimed by the creator after `unlock_time` via `ClaimCreatorExcessLiquidity`. |
| Post-threshold commit still available, kicking out 1% bluechip + 5% creator fee? | **Yes.** Fees are split in the dispatcher for **every** path; post-threshold the net-of-fees remainder is swapped for creator tokens via `MsgSwapExactAmountIn` and forwarded to the committer. |
| Is commit the only tx available pre-threshold? | **Yes.** `SimpleSwap`, `ClaimCreatorExcessLiquidity`, `ContinueDistribution`, etc. are all gated on `IS_THRESHOLD_HIT` / post-crossing state. Only `Commit` (and factory-admin ops) work pre-threshold. |
| Fully compatible with Osmosis? | **Mostly.** Message construction (tokenfactory/gamm/poolmanager/twap via osmosis-std 0.27) is correct. Two caveats: the GAMM creation-fee provisioning (H-01) and missing denom metadata (M-01). |
| Will explorers pick up the pools & transactions? | **Pools: yes** (native GAMM pool). **Swaps: yes**, but attributed to the *pool contract* as `sender`, followed by a separate `BankMsg::Send` to the user (explorer shows the contract swapping, not the EOA). **Creator token: shows as a denom but with no name/symbol/decimals** until M-01 is fixed. |

---

## Findings

### [HIGH] H-01 — GAMM `PoolCreationFee` under-provisioning bricks threshold crossing and permanently locks committed funds
- **Severity:** High (escalates to **Critical** if deployed with the default/zero fee)
- **Category:** Availability / Fund Safety / External-module integration
- **Files:**
  - `creator-pool/src/commit/threshold_payout.rs:236-341` (seed/fee math, `reply_on_success` create)
  - `factory/src/state.rs:170-182` (field), `:233-239` (`default_gamm_pool_creation_fee` = **zero coin**)
  - `factory/src/execute/config.rs:26-143` (`validate_factory_config` — **never validates this field**)
  - `packages/pool-core/src/osmosis_msgs.rs:99-125` (`create_balancer_pool_msg`)
- **Description:** At crossing, the pool emits `SubMsg::reply_on_success(MsgCreateBalancerPool, REPLY_ID_CREATE_POOL)`. On Osmosis the `x/gamm` module deducts `PoolCreationFee` (currently **1000 OSMO**, a governance param) from the sender's balance *in addition to* the seeded `pool_assets`. The contract funds this from `BLUECHIP_FEE_RESERVED`, filled by retaining 1% of each commit toward `CREATION_FEE_RESERVE_TARGET`, which is threaded from `factory_config.gamm_pool_creation_fee.amount`. That config value **defaults to zero** and is **never validated** — not against the chain's live `PoolCreationFee`, not even for denom. I verified the balance invariant: the create bricks iff `real_chain_fee > configured_gamm_fee` (the reserve is capped at the configured target, so the configured value is the binding bound). Because the SubMsg is `reply_on_success`, a failed create reverts the **entire** threshold-crossing transaction.
- **Attack / failure scenario:**
  1. Factory is deployed with `gamm_pool_creation_fee` unset (default zero) or set below the live 1000-OSMO `PoolCreationFee` — or is set correctly, and later Osmosis governance raises `PoolCreationFee`.
  2. Pools accept pre-threshold commits normally; committers send real OSMO (net of fees enters the pool).
  3. A commit crosses the $25k threshold → `trigger_threshold_payout` runs → `MsgCreateBalancerPool` is dispatched.
  4. `x/gamm` charges the real fee; the pool holds `seed_osmo + configured_fee` at most, which is `< seed_osmo + real_fee` → **create reverts → whole crossing tx reverts.**
  5. Every subsequent crossing attempt reverts identically. The pool is frozen just below threshold **forever.**
- **Impact:** Permanent, unrecoverable. The pool can never cross, so swaps never open; and because pre-threshold emergency withdraw is explicitly disabled (see H-02) with no refund path, **all committed OSMO for that pool's committers is permanently locked.** With the default (zero) value this affects **100% of pools** on mainnet.
- **Recommendation:**
  1. In `validate_factory_config`, reject a `gamm_pool_creation_fee` whose denom ≠ `bluechip_denom` or whose amount is below a deployment-required floor; strongly consider **querying `x/gamm`'s `Params`** (or `x/poolmanager` pool-creation-fee) at instantiate/propose time and requiring `configured >= live_fee` (the same live-probe pattern already used for the TWAP route in `config.rs:107-116`).
  2. Add a defensive margin (e.g., `configured >= live_fee * 1.2`) so a modest governance bump doesn't brick in-flight pools.
  3. Pair with H-02 so that even if a crossing can't complete, committed funds are recoverable.
- **References:** Osmosis `x/gamm` `PoolCreationFee` param; CWA-2022 class "external module fee not provisioned."

---

### [HIGH] H-02 — Pre-threshold committed funds have no refund/withdrawal path (permanent lock if a pool never crosses)
- **Severity:** High *(flag for product confirmation — may be intended bonding-curve semantics, but the lock is real and unrecoverable)*
- **Category:** Fund Safety / Design
- **Files:** `creator-pool/src/admin.rs:76-83` (pre-threshold emergency withdraw explicitly disabled); `creator-pool/src/contract.rs:216-312` (execute dispatch — no refund/withdraw variant); `creator-pool/src/commit/pre_threshold.rs` (funds banked, never returned)
- **Description:** Every pre-threshold commit sends net OSMO into the pool's bank balance and records the committer in `COMMIT_LEDGER`. The only ways funds leave a pre-threshold pool are (a) threshold crossing, or (b) emergency withdraw — and (b) is explicitly rejected pre-threshold: `admin.rs:81` returns `EmergencyWithdrawPreThreshold`, with a code comment acknowledging "The correct recovery path for a pre-threshold pool is a future cancel/refund flow; until that exists, refuse to run emergency withdraw." No such flow exists. There is no `RefundCommit`, no per-committer withdrawal, nothing.
- **Attack / failure scenario:** No attacker needed. Most token launches fail to reach their raise target. A pool that never hits $25k (creator abandons it, insufficient interest, or H-01 bricks the crossing) leaves every committer's net OSMO permanently stranded in a contract they cannot withdraw from and an admin cannot drain.
- **Impact:** Guaranteed permanent fund lock under a common, non-adversarial scenario. This is also what makes H-01 catastrophic rather than merely a liveness bug.
- **Recommendation:** Add a pre-threshold committer refund path: either a per-committer `WithdrawCommit` that returns their `COMMIT_LEDGER` balance (net) and decrements the raise, or an admin/permissionless `CancelPool` that, before crossing, refunds all committers pro-rata and marks the pool dead. If permanent lock *is* the intended economic model, document it prominently and surface it in the commit UX — but for a board submission the invariant "committed funds can always eventually be recovered" should hold or be an explicit, acknowledged design decision.

---

### [MEDIUM] M-01 — Creator token denom metadata (name / symbol / decimals / display) is never set on-chain
- **Severity:** Medium (functional/UX; directly defeats a stated product requirement)
- **Category:** Osmosis integration / correctness
- **Files:** `packages/pool-core/src/osmosis_msgs.rs:52-58` (`create_denom_msg` — only `MsgCreateDenom`); `creator-pool/src/contract.rs:112-114` (denom created, no metadata); `factory/src/execute/pool_lifecycle/create.rs:94-148` (`CreatorTokenInfo` validated but `name`/`decimals` used only to gate the subdenom)
- **Description:** The pool sends `MsgCreateDenom` to register `factory/{pool}/{subdenom}` but **never** sends `MsgSetDenomMetadata`. A repo-wide search confirms no `SetDenomMetadata` anywhere in contract code. The validated `CreatorTokenInfo { name, symbol, decimal }` is consumed only to derive `subdenom = symbol.to_lowercase()` (`create.rs:40-42`). Consequently the bank module has no `Metadata` for the denom: no human-readable `name`, no `symbol`/`display` ticker, and crucially **no `denom_units`/`exponent`**, so wallets and explorers cannot render the 6-decimal scaling — the token displays as a raw `factory/{addr}/{sub}` micro-denom.
- **Impact:** Directly contradicts "do creators still get to name their token." Explorers "pick up" the denom but show it un-named and un-scaled; frontends/wallets that rely on bank metadata (most do) show garbage. No fund risk.
- **Recommendation:** After `MsgCreateDenom`, emit `MsgSetDenomMetadata` (available in `osmosis-std` tokenfactory) populated from the already-validated `CreatorTokenInfo`: `name`, `symbol` (as `display`), and `denom_units` with `exponent = 6`. This can ride the same instantiate response as the create-denom message (the pool is the admin).

---

### [MEDIUM] M-02 — "Estimate-derived" slippage floor provides no real sandwich protection when `belief_price` is omitted
- **Severity:** Medium
- **Category:** Oracle/price trust / MEV
- **Files:** `packages/pool-core/src/swap.rs:60-210` (`derive_token_out_min`, `compute_token_out_min`, `estimate_swap_out`); post-threshold commit path `creator-pool/src/commit/post_threshold.rs:79-87`
- **Description:** `token_out_min = max(estimate_floor, belief_floor)`, where `estimate_floor = estimated_out * (1 - max_spread)` and `estimated_out` is the poolmanager's quote **at current pool state**. The code comments describe this as closing the "no belief price ⇒ no sandwich/slippage protection" hole. It does not: a front-runner moves the pool *before* the victim's tx, so `estimated_out` is computed against the **already-degraded** state and the resulting floor is satisfied by the sandwiched fill. Only a caller-supplied `belief_price` (an off-chain reference the attacker can't move) yields genuine protection — and it is optional. The estimate floor only prevents dispatching against a *stale/zero* quote; it is not anti-sandwich.
- **Attack / failure scenario:** Attacker sees a `SimpleSwap`/post-threshold `Commit` with no `belief_price` in the mempool → front-runs to push the pool price against the victim → victim's contract queries the estimate (now reflecting the pushed price) → `token_out_min` is set low enough that the victim's swap succeeds at the bad price → attacker back-runs. Classic sandwich; the "protection" is cosmetic.
- **Impact:** Users trusting the default max_spread with no belief price can be sandwiched on every swap and post-threshold commit. Value loss bounded by max_spread per hop but repeatable.
- **Recommendation:** Either (a) require a non-zero `belief_price` for user-facing `SimpleSwap`/commit swaps, or (b) rename/re-comment the estimate floor as "liveness/zero-quote guard, NOT anti-sandwich" and document that sandwich protection requires `belief_price`. Frontends should always pass a belief price sized from an independent quote.

---

### [MEDIUM] M-03 — Router sweeps its entire balance per hop; stray/donated funds are claimable by an arbitrary caller
- **Severity:** Medium
- **Category:** Fund Safety / accounting invariant not enforced
- **Files:** `router/src/execution.rs:238-265` (hop input = full router balance), `:26-34` (documented but unenforced "zero balance between txs"), `:491-505` (`extract_native_offer` checks coin count + denom, **not amount**)
- **Description:** `execute_swap_operation` ignores the route-threaded offer amount and swaps the router's **entire current bank balance** of the offer denom (`query_pool_strict(env.contract.address)`). The safety of this rests on the un-enforced invariant "the router holds zero balance between transactions." Nothing enforces it — anyone can `MsgSend` to the router, and pool edge-cases can leave dust. `extract_native_offer` only requires exactly one attached coin of the right denom, not a specific amount.
- **Attack / failure scenario:** A user mis-sends (or an attacker donates) `D` of denom `X` to the router → any caller submits `ExecuteMultiHop` whose first hop offers `X`, attaching 1 unit → hop 0 sweeps `D+1` and delivers the output to the caller. Funds are claimable by whoever routes that denom first, never the sender. No theft of *in-flight* funds (execution is atomic), which caps this at Medium.
- **Impact:** Any balance that lands in the router is claimable by an arbitrary caller.
- **Recommendation:** Thread the explicit per-hop input: hop 0 uses the extracted `offer_amount`; hops 1..N use the previous hop's exact `return_amount` (capture via `reply_on_success` or assert `balance_delta == expected`). At minimum, sweep only the delta credited during *this* route.

---

### [MEDIUM] M-04 — Factory `migrate()` omits the cw2 contract-name check
- **Severity:** Medium
- **Category:** Migration safety
- **Files:** `factory/src/migrate.rs` (version-only comparison, then `set_contract_version` unconditionally)
- **Description:** `migrate` compares only the stored **version** semver, never `stored.contract == CONTRACT_NAME`. Standard cw2 hygiene (`ensure_eq!(stored.contract, CONTRACT_NAME)`) is missing, so migrating this code id onto a different contract's storage (or an operator pointing migrate at the wrong instance) passes the guard, overwrites the name, and reinterprets foreign storage as factory state — including the registry back-fill loop that then walks arbitrary bytes.
- **Impact:** Silent state corruption on operator/governance error. No fail-safe.
- **Recommendation:** Add `if stored_version.contract != CONTRACT_NAME { return Err(...) }` before the version comparison.

---

### [MEDIUM] M-05 — Factory `migrate()` performs an unbounded O(N) registry back-fill, re-run on every migration
- **Severity:** Medium
- **Category:** Upgrade liveness / DoS
- **Files:** `factory/src/migrate.rs` (collects all `POOLS_BY_ID` keys, loops with load + up to two `may_load`+`save` per pool, no cursor, no completion flag; equal-version re-runs allowed)
- **Description:** Every migration walks the entire pool registry to back-fill `PAIRS`/reverse-index, with no batching and no one-time "already back-filled" gate. Pool count grows unbounded over the factory's life (the 1h/address create rate-limit only slows growth; an attacker can rotate funded addresses). Once N is large enough that the back-fill exceeds the migration gas ceiling, `migrate` can never complete — **the factory becomes permanently un-upgradeable.**
- **Impact:** Loss of upgradeability for a long-lived factory.
- **Recommendation:** Gate the back-fill behind a one-time `Item<bool>` completion flag so re-runs skip it, and/or make it resumable/paginated via a bounded follow-up admin call.

---

### [LOW] L-01 — `register_pool` duplicate guard covers only `PAIRS`; the other three maps are blind overwrites
- **Severity:** Low
- **Files:** `factory/src/state.rs:345-388`
- **Description:** The uniqueness guard checks only `PAIRS`. `POOLS_BY_ID`, `POOL_ID_BY_ADDRESS`, `POOLS_BY_CONTRACT_ADDRESS` are unconditional `save`s. Not exploitable today (pool_id from monotonic counter, deterministic address), but a future counter/finalize regression would silently overwrite prior registry entries.
- **Recommendation:** `ensure!(!POOLS_BY_ID.has(...))` and `ensure!(!POOL_ID_BY_ADDRESS.has(...))` inside `register_pool`; assert `pool_address == pool_details.creator_pool_addr`.

### [LOW] L-02 — Router execution omits the `IsFullyCommited` check that simulation performs
- **Severity:** Low
- **Files:** `router/src/execution.rs:360-391` vs `router/src/simulation.rs:75-87`
- **Description:** Simulation rejects a route through a pre-threshold pool with `PoolInCommitPhase`; execution relies on the pool to reject, surfacing an opaque `HopFailed` instead. Atomic revert, no fund loss, but simulate/execute disagree.
- **Recommendation:** Add the `IsFullyCommited` query to `validate_route_pools_registered`.

### [LOW] L-03 — Router single-step admin rotation can permanently lock config control
- **Severity:** Low
- **Files:** `router/src/contract.rs:129-131`, `:173-175`
- **Description:** Admin rotation validates only bech32 form, not control; a valid-but-wrong address bricks all future config mutation (propose/apply/cancel all gate on `config.admin`). Router custodies no funds, so blast radius is `factory_addr` immutability.
- **Recommendation:** Two-step handover (`AcceptAdmin`).

### [LOW] L-04 — Factory config apply re-runs a live external TWAP probe (griefable delay)
- **Severity:** Low
- **Files:** `factory/src/execute/config.rs:107-116`, `:165`
- **Description:** `execute_update_factory_config` re-runs `probe_native_usd_rate` at apply time; a third party who degrades/prunes the pricing pool during the 48h window can force apply to revert, delaying a legitimate config change ≥48h per attempt. Fail-closed is intended, so this is a tradeoff.
- **Recommendation:** Split validation into structural (hard-fail at apply) vs live-probe (hard-fail only at propose; warn at apply).

### [LOW] L-05 — Factory upgrades: doc claims an "anchor-exclusion" safeguard that the code does not implement
- **Severity:** Low (documentation vs code mismatch)
- **Files:** `factory/src/execute/upgrades.rs:60`, `:122-134` (doc) vs `:157-213` (code)
- **Description:** `build_upgrade_batch`'s doc claims it re-resolves an "anchor" and hard-fails if it appears in the batch. No anchor concept exists anywhere in factory state and no such check runs. Harmless today, but misleads maintainers and would silently permit migrating an anchor pool if one is ever introduced.
- **Recommendation:** Implement the described check or delete the stale comment.

---

## Informational

- **I-01 — Stale doc on `FactoryInstantiate.gamm_pool_creation_fee`** (`factory/src/state.rs:170-182`): the comment says the fee is "collected from the creator at Create time and forwarded into the pool's instantiate funds," but FIX-E changed this — the pool is instantiated with `funds: vec![]` and the fee is retained from the 1% commit stream. Misleading for integrators/reviewers.
- **I-02 — `query_creator_token_info` masks a bank-query error as zero supply** (`factory/src/query.rs:194-198`): `unwrap_or_else(|_| zero)` hides query failures; propagate with `?`.
- **I-03 — `NotifyThresholdCrossed.crossed_at` is caller-supplied and only echoed as an event attribute** (`factory/src/execute/pool_lifecycle/admin.rs:195-200`): the doc implies it drives minting; it does not (the pool mints off its own `env.block.time`). Off-chain consumers should treat the factory's `crossed_at` attribute as pool-asserted, not authoritative.
- **I-04 — Router `bluechip_denom` is stored, immutable, and never used in routing** (`router/src/state.rs:37`); dead config.
- **I-05 — Serde-default schema footgun:** `FactoryInstantiate` / `CommitLimitInfo` / `DistributionState` schema evolution relies entirely on `#[serde(default)]`; any future non-defaulted field bricks deserialization post-migration since `migrate` never load-rewrites these items. Enforce "every new field carries a serde default" in CI.
- **I-06 — Existence check precedes auth** in `execute_recover_pool_stuck_states` (`factory/src/execute/pool_lifecycle/admin.rs:130-135`) — trivial pool-existence oracle for non-admins.
- **I-07 — Swap attribution:** post-threshold swaps/commits execute with the *pool contract* as `MsgSwapExactAmountIn.sender` and forward output via `BankMsg::Send`. Explorers attribute the swap to the contract, not the end user — a UX/indexing note, not a bug.

---

## Invariant Verification

| Invariant | Holds? | Notes |
|---|---|---|
| No double-mint / double-seed at crossing | **Yes** | `IS_THRESHOLD_HIT` gate is the single load-bearing check in `trigger_threshold_payout`; set only after all mint/seed work is scheduled; re-entry blocked by `REENTRANCY_LOCK` and `THRESHOLD_PROCESSING`. |
| `pool_osmo_balance == pools_bluechip_seed + reserved` through crossing | **Yes** | Verified algebraically across fees → refund → seed → creation-fee → remit for both the no-cap and over-cap branches; no brick on the balance side, earmark fully backed. |
| Threshold-payout splits are canonical (325B/25B/350B/500B = 1.2T) | **Yes** | Enforced at factory config, at pool instantiate, and again at runtime in `trigger_threshold_payout`. |
| Committer distribution conserves supply | **Yes** | Floor-division dust is settled to the creator on the final batch, gated on `distributed_so_far > 0` to avoid double-mint on legacy state. |
| Native GAMM pool created iff threshold crossed | **Conditionally** | Holds logically, **but** the create can revert and brick the crossing under H-01. |
| Creator excess always backed by contract balance | **Yes** | FIX-C/FIX-E leave exactly `excess_bluechip` after seeding + fee; drain excludes the earmark (`saturating_sub`). |
| Pre-threshold funds recoverable | **NO** | H-02 — no exit path. |
| One pool per unordered pair | **Yes** | `canonical_pair_key` + `PAIRS` guard; creator denom is per-pool unique so collisions are structurally impossible. |
| `NotifyThresholdCrossed` callable only by the registered pool, once | **Yes** | Address check + `POOL_THRESHOLD_CROSSED` idempotency gate. |

---

## Attack Surface Summary

| Entry point | Contract | Auth | Risk |
|---|---|---|---|
| `Create` | factory | permissionless (fee + rate-limit) | Low |
| `Propose/Apply/Cancel *Config`, `UpgradePools`, `Pause/Unpause`, `EmergencyWithdraw*`, `Recover*` | factory | admin-only (48h timelock on config/upgrade) | Low (auth + timelock verified) |
| `NotifyThresholdCrossed` | factory | registered-pool-only + idempotent | Low |
| `PruneRateLimits` | factory | permissionless | Low (bounded batch) |
| `migrate` | factory | wasmd admin | **Medium** (M-04/M-05) |
| `Commit` | creator-pool | permissionless | **High** (H-01/H-02 lifecycle risk; M-02 MEV) |
| `SimpleSwap` | creator-pool | permissionless, post-threshold | **Medium** (M-02 sandwich) |
| `ContinueDistribution` / `SelfRecoverDistribution` / `ClaimFailedDistribution` | creator-pool | permissionless (rate-limited) | Low |
| `ClaimCreatorExcessLiquidity` | creator-pool | creator-only + timelock | Low |
| `UpdateConfigFromFactory` / admin ops | creator-pool | factory-only | Low |
| `ExecuteMultiHop` | router | permissionless | **Medium** (M-03 sweep) |
| `ExecuteSwapOperation` / `AssertReceived` | router | self-only | Low |

---

## Recommendations Summary (highest severity first)

1. **H-01** — Validate/provision the GAMM `PoolCreationFee`: reject a zero/under-set `gamm_pool_creation_fee`, ideally live-probe `x/gamm` params at config time and require `configured >= live_fee` with margin. **Blocking for mainnet.**
2. **H-02** — Add a pre-threshold committer refund / pool-cancel path so committed funds are always recoverable. **Blocking for mainnet** (or an explicit, documented design acceptance).
3. **M-01** — Emit `MsgSetDenomMetadata` from `CreatorTokenInfo` (name/symbol/display/6-decimal units). Required to satisfy the "creators name their token" + explorer requirements.
4. **M-02** — Require `belief_price` for user-facing swaps/commits, or stop representing the estimate floor as anti-sandwich.
5. **M-03** — Thread explicit per-hop amounts in the router instead of sweeping the whole balance.
6. **M-04 / M-05** — Add the cw2 name check to `migrate`; gate/paginate the registry back-fill.
7. **L-01…L-05, I-01…I-07** — Hardening and documentation cleanup as detailed above.

---

*Methodology: full file-tree map; read every entry point (`instantiate`/`execute`/`query`/`migrate`/`reply`) across all crates; traced every funds-touching and privileged path and the Osmosis message construction; category-by-category review (reentrancy, access control, arithmetic, atomicity, fund accounting, oracle trust, DoS, init/migration, reply safety, input validation); grep sweeps for `unwrap/expect/unchecked/panic/TODO`. No production `unwrap()/expect()/panic!` found; `overflow-checks = true` in release.*
