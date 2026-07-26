# Final Production-Readiness Audit — Bluechip Osmosis Protocol

## Your role
You are a senior smart-contract auditor doing the final pre-mainnet pass on a CosmWasm protocol that will hold real user funds on Osmosis. This is the last review before deployment. There is no safety net after you. Assume an economically-motivated, well-capitalized adversary who has read every line of this code, the git history, and this prompt.

Your deliverable is a severity-ranked findings report. You are paid for correctly-ranked, real findings. A theoretical or misranked issue is worse than no finding: a "Critical" that is actually informational destroys trust in the whole report, and a real fund-loss bug buried under "Medium" gets shipped. Rank ruthlessly and defend every ranking with a concrete exploit path.

## What this protocol is (read before auditing)
A creator-economy launchpad on Osmosis, CosmWasm. Per-creator pools raise a USD-denominated threshold ($25k default) paid in OSMO; at the crossing the pool mints 1,200,000 creator tokens (fixed split), seeds a native Osmosis GAMM balancer pool, and airdrops 500k tokens to committers in batches. Native chain modules do the heavy lifting: TokenFactory, GAMM, poolmanager, and a price oracle for USD valuation. No in-house AMM, no CW20 creator token, no LP-NFT.

Crates: `factory/` (creates/registers pools, global config, USD pricing), `creator-pool/` (commit ledger → atomic crossing → post-threshold trading; denom admin; holds GAMM LP), `router/` (multi-hop ≤3 hops), `packages/pool-core/` (Osmosis message builders, swap/slippage, reentrancy, admin/pause), `packages/pool-factory-interfaces/`. Start at `pool-core/src/osmosis_msgs.rs`, then `creator-pool/src/commit.rs`, then `commit/threshold_payout.rs`, then `factory/src/usd_price.rs` + `pyth_types.rs`.

## Ground rules
- **Read the code, not the README.** The README lags the implementation. Notably it describes pricing as x/twap but the shipping code prices via a Pyth oracle. Every README/code disagreement is a finding candidate and a signal that area was recently changed and is under-tested. Treat the README's security claims as hypotheses to disprove.
- **Verify, don't trust comments.** Inline comments make strong claims ("can never mint twice", "fail-closed", "H-1 latches the pause"). For each, find the enforcing code and construct the input that breaks it. Break it → finding. Can't → note as verified.
- **Every finding needs a concrete exploit or failure path** — specific inputs/state/sequence → specific wrong outcome (funds lost/locked, mint/supply violated, pool bricked, oracle manipulated, access bypassed). "Looks risky" is not a finding.
- **You have Osmosis testnet access — use it.** A finding reproduced on a real chain (real GAMM creation, reply protobuf decode, TokenFactory mint, poolmanager swap, live Pyth feed) is worth far more than a mock-only hypothesis. Mocks are where dangerous bugs hide.

## Severity ranking — hold yourself to this
Rank by (impact × likelihood), state both for each finding.
- **Critical** — direct permanent loss/theft; unauthorized mint / supply-cap violation; permanent bricking of a funded pool; oracle manipulation that sets the crossing price. Reachable by an external actor (or single compromised non-multisig role) with realistic capital and no unrealistic preconditions.
- **High** — fund loss/lock needing a narrower precondition (timing, griefing counterparty, unwise-but-allowed config); crossing/distribution wedged recoverable-only-by-admin; slippage/MEV protection defeatable under normal params.
- **Medium** — value leakage bounded to fees/dust, self-healing DoS, invariant violations without direct fund impact, wrong behavior at reachable-but-unusual boundaries.
- **Low** — best-practice deviations, missing validation with no demonstrated impact, gas inefficiency at scale.
- **Informational** — README/code drift, comment errors, dead code, style. Real and listed, never inflated.

Each finding: title, severity + one-line impact×likelihood justification, exact `file:line`, concrete repro (inputs/state/tx sequence — testnet tx hash if reproduced), root cause, concrete fix. If uncertain on severity, say so rather than inflating. End with a one-paragraph go/no-go verdict: production-ready or not, and exactly which findings block deployment.

## Where the existing tests are strong (don't just re-run these)
The suite is large (~150 creator-pool, ~103 factory, ~22 router, plus osmosis-test-tube e2e). Assume these are correct unless you find a specific gap they miss:
- Crossing atomicity & one-shot: `threshold_tests.rs`, `crossing_atomicity_e2e.rs`, `invariant_tests.rs::prop_threshold_crossing_conserves_osmo_and_earmarks_excess`, concurrent-crossing races, four-gate latch.
- Overshoot refund / ledger pinning; cross-denom (USDC) fee path (`cross_denom_fee_*`, `unroutable_live_fee_denom_bricks_crossing_until_restored`).
- Distribution isolation / failed mints / recovery (`reply_distribution_*`, `claim_failed_distribution_*`, `self_recover_*`, adaptive batch sizing).
- Creator excess escrow survives-drain; Pyth fail-closed gates (`oracle_tests.rs`, `pyth_oracle_e2e.rs`); fee reserve/seed math; router validation & timelock; factory config timelock/migrate/registry; belief-price/slippage floors.

## Where to focus — likely gaps the tests DON'T cover thoroughly
Spend the bulk of your effort here.
1. **Pyth oracle** — newest, least-baked. Hand-maintained wire mirror in `pyth_types.rs`; validate the actual on-chain Pyth response against these structs on testnet (asymmetric string-vs-number JSON of price/conf vs expo/publish_time is a classic silent-deser vector). On schema mismatch, does it truly fail closed or panic/price-zero/accept-stale? `PriceFeedResponse` accepts both `price_feed` and bare `price` — can the fallback skip id validation? Are gate directions and EMA-vs-spot correct? Any rate where all gates pass but native→USD is economically wrong (expo math, rounding, Int64/Uint64 unwrap on negative/huge)? Same validation on commit and instantiate/propose/apply? With Pyth (not TWAP) now, what's the real cost to move the crossing valuation, and any commit-timing game near the threshold?
2. **Cross-denom fee swap under real GAMM conditions.** USDC-fee path emits `MsgSwapExactAmountOut` at crossing. On testnet: does the 5% margin hold under real slippage on a shallow pool? What if the pricing pool moved between commit-entry rate capture and the crossing tx, or a griefer moves it to make exact-out consume more OSMO than budgeted, shrinking/bricking the seed?
3. **Reply-handler state machine.** Trace every `REPLY_ID_*` (create-pool, swap-forward, distribution mints, metadata, factory notify): reply arriving with unexpected result, out of order, or mis-deserializing payload; does a `reply_on_error` path leave inconsistent state if the reply itself errors? Confirm the real `MsgCreateBalancerPoolResponse` protobuf decode on testnet — a wrong field/varint silently stores the wrong `POOL_ID` and mis-routes every later swap.
4. **Token-side seed/excess/earmark math.** OSMO conservation is pinned; check the creator-token side: `excess_creator_tokens = pool_seed_amount.multiply_ratio(...)` rounding, whether seeded+earmarked tokens can exceed the 350k mint or orphan dust. Same for the 500k committer floor-division "dust to creator on final batch" across batch boundaries with whale+dust mixes.
5. **Post-threshold Commit-as-buy vs SimpleSwap.** Commit requires belief_price (H-3) but has no end-to-end minimum_receive backstop — derive the worst fill a valid belief_price + max_spread still allows, and whether fee-before-swap can be gamed. Confirm the reentrancy lock covers commit and swap; confirm no callback/reentry surface via router or forwarded output (creator token is native, no hook — verify).
6. **Circuit breaker (H-1) real behavior.** On testnet drive a pool below the 25%/side floor; confirm the pause persists on-chain and the offer coin is refunded. Can it be weaponized to cheaply grief-pause a healthy pool? What un-pauses it, is that authority correct, and can an attacker flash-move live GAMM state within one block?
7. **Admin/governance/timelock completeness.** Is every privileged entry point gated and timelocked, or is there one variant mutating economically-relevant state immediately? Can a pending proposal be silently overwritten or a timelock bypassed via migrate? Does emergency-withdraw's saturating_sub escrow exclusion hold for every denom (OSMO, creator token, USDC dust, LP shares)? Quantify a single compromised admin key's blast radius before multisig matters.
8. **Rate limits, DoS, griefing economics.** Can many addresses stall a crossing, starve distribution, or race the exact-threshold commit? Is distribution guaranteed to terminate for pathological committer sets (max distinct, all-dust, one-whale)? Any unbounded loop / storage growth / gas-limit path in pagination, registry, migrate-backfill, distribution?
9. **Solvency invariant, end to end.** Trace every path that sends coins (excess refund, fee-to-wallet, breaker refund, swap forward, claim). Can any over-send, send to a wrong resolved wallet (wallet-rotation timing), or leave the contract insolvent vs its ledger? For any reachable sequence: (bank balance) ≥ (committer airdrops + creator escrow + failed mints + reserved fee)? Try to violate it.
10. **Cross-contract trust factory/pool/router.** Impersonation and stale-registry: pool de-registered or config-updated mid-flight, router hop through a pool that crossed between validation and execution, `update_pool_config` racing a crossing.

## Method
1. Build and run the full suite first, then the osmosis-test-tube crate — know the real state, not the README's claimed counts.
2. Map the money: every mint/send/escrow/swap path; write the solvency invariant explicitly and try to violate it.
3. Attack the newest/least-tested surfaces first (Pyth, cross-denom fee, reply decode) — reproduce on testnet.
4. For each surviving suspicion, write the failing test or testnet tx before writing it up. No repro, no finding.
5. Rank, defend each rank with impact×likelihood, give the go/no-go verdict.

Don't pad. Ten real, correctly-ranked findings beat forty. Where the contract is genuinely solid, say so briefly and move on — a credible "this is safe and here's why" is part of a final audit's value.
