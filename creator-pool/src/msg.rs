//! Creator-pool (commit-phase) wire-format types.
//!
//! Shared types (response structs, CommitFeeInfo, PoolConfigUpdate,
//! Cw20HookMsg, CommitStatus) live in `pool_core::msg` and are
//! re-exported below so every existing `use crate::msg::X;` import in
//! the creator-pool crate resolves unchanged.
//!
//! Per-contract types — the ExecuteMsg / QueryMsg / MigrateMsg /
//! PoolInstantiateMsg enums and the commit-only response types
//! (FactoryNotifyStatusResponse, PoolCommitResponse, CommitterInfo,
//! LastCommittedResponse) — stay here.
pub use pool_core::msg::*;

use crate::asset::{TokenInfo, TokenType};
use crate::state::RecoveryType;
// Schema-only refs: cited only by `#[returns(...)]` on QueryMsg
// variants. The QueryResponses derive consumes them but rustc still
// flags them as unused without this allow.
use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Binary, Decimal, Timestamp, Uint128};
#[allow(unused_imports)]
use {
    crate::state::{Committing, PoolDetails},
    pool_factory_interfaces::{AllPoolsResponse, PoolStateResponseForFactory},
};

#[cw_serde]
pub enum ExecuteMsg {
    // The creator token is a native TokenFactory denom now, so selling it
    // is a normal `SimpleSwap` with the creator denom attached as funds —
    // there is no CW20 `Receive(Cw20ReceiveMsg)` sell hook anymore.
    SimpleSwap {
        offer_asset: TokenInfo,
        belief_price: Option<Decimal>,
        max_spread: Option<Decimal>,
        #[serde(default)]
        allow_high_max_spread: Option<bool>,
        to: Option<String>,
        transaction_deadline: Option<Timestamp>,
    },
    UpdateConfigFromFactory {
        update: PoolConfigUpdate,
    },
    RecoverStuckStates {
        recovery_type: RecoveryType,
    },
    ContinueDistribution {},
    Pause {},
    Unpause {},
    EmergencyWithdraw {},
    Commit {
        asset: TokenInfo,
        transaction_deadline: Option<Timestamp>,
        belief_price: Option<Decimal>,
        max_spread: Option<Decimal>,
    },
    // Time-locked release of the creator's RAW excess coins earmarked at
    // threshold crossing (see `CreatorExcessLiquidity`). Sends the raw
    // `bluechip_amount` + `token_amount` to the creator once `unlock_time`
    // has passed (FIX C). Claimable even after an emergency drain (FIX D).
    ClaimCreatorExcessLiquidity {
        #[serde(default)]
        transaction_deadline: Option<Timestamp>,
    },
    // Re-sends NotifyThresholdCrossed to the factory when the initial
    // notification during threshold-crossing failed and PENDING_FACTORY_NOTIFY
    // is set. Anyone can call: factory's POOL_THRESHOLD_CROSSED
    // idempotency check gates double-processing, so at worst a stray
    // caller burns gas on a no-op. Clears the pending flag on
    // successful reply.
    RetryFactoryNotify {},
    CancelEmergencyWithdraw {},

    // Permissionless distribution restart for the catastrophic case where
    // the admin path is unavailable for an extended period. Available
    // only after PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS (7 days)
    // since the last successful batch — the admin's 1h window has many
    // chances to fire first. Restarts the cursor at None and resets
    // failure counters; preserves `distributed_so_far` so dust settlement
    // still mints exactly the post-distribution residual.
    SelfRecoverDistribution {},

    // Withdraw a failed distribution mint. Caller must have a
    // non-zero entry in FAILED_MINTS (the original committer address).
    // Optional `recipient` lets the user route the claim to a fresh
    // wallet — useful when the original recipient is the reason the mint
    // failed (e.g., a contract that rejects CW20 receive). Defaults to
    // `info.sender` so the simple case requires no parameters.
    //
    // Mint is dispatched as a reply_always SubMsg using the same
    // isolation harness as the bulk distribution path: if it fails again
    // (e.g., the alternate recipient is also blocked) the amount is
    // re-stashed into FAILED_MINTS under the original committer address
    // so they can try again with yet another recipient.
    ClaimFailedDistribution {
        recipient: Option<String>,
    },
}

#[cw_serde]
pub enum MigrateMsg {
    /// Tune `PoolSpecs.lp_fee` to `new_fees`. Accepted range:
    /// `MIN_LP_FEE` (0.1% / `Decimal::permille(1)`) up to
    /// `MAX_LP_FEE` (10% / `Decimal::percent(10)`) inclusive. Values
    /// outside this range are rejected at runtime with
    /// `ContractError::LpFeeOutOfRange`. The schema accepts any
    /// `Decimal` so client tooling that wants to encode the bounds
    /// must do so out-of-band; the runtime gate is authoritative.
    UpdateFees { new_fees: Decimal },
    /// No-op variant. Bumps the cw2 stored version on a successful
    /// migrate without touching any other state. Use when the only
    /// change between releases is the wasm code id.
    UpdateVersion {},
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(PoolDetails)]
    Pair {},
    #[returns(ConfigResponse)]
    Config {},
    #[returns(SimulationResponse)]
    Simulation { offer_asset: TokenInfo },
    #[returns(FeeInfoResponse)]
    FeeInfo {},
    #[returns(CommitStatus)]
    IsFullyCommited {},
    #[returns(Option<Committing>)]
    CommittingInfo { wallet: String },
    #[returns(PoolCommitResponse)]
    PoolCommits {
        pool_contract_address: Addr,
        min_payment_usd: Option<Uint128>,
        after_timestamp: Option<u64>,
        start_after: Option<String>,
        limit: Option<u32>,
    },
    #[returns(PoolStateResponse)]
    PoolState {},
    #[returns(PoolFeeStateResponse)]
    FeeState {},
    // NOTE: the canonical wire name carries an on-chain typo
    // ("last_commited"). The serde alias also accepts the correct
    // spelling so new clients don't have to ship the typo; renaming
    // outright would break every deployed integration.
    #[returns(LastCommittedResponse)]
    #[serde(alias = "last_committed")]
    LastCommited { wallet: String },
    #[returns(PoolInfoResponse)]
    PoolInfo {},
    #[returns(PoolAnalyticsResponse)]
    Analytics {},
    #[returns(PoolStateResponseForFactory)]
    GetPoolState {},
    #[returns(AllPoolsResponse)]
    GetAllPools {},
    #[returns(pool_factory_interfaces::IsPausedResponse)]
    IsPaused {},
    // Reports whether a NotifyThresholdCrossed-to-factory notification
    // is pending retry (see PENDING_FACTORY_NOTIFY / RetryFactoryNotify).
    // Useful for keepers and ops dashboards watching for stuck pools.
    #[returns(FactoryNotifyStatusResponse)]
    FactoryNotifyStatus {},
    // Reports the live state of post-threshold committer payouts so admin
    // dashboards can detect a stalled distribution. Returns `None` when
    // no distribution is active (pre-threshold, or fully completed and
    // cleaned up). Returns `Some(...)` with a computed `is_stalled` flag
    // (true when the per-pool 24h DISTRIBUTION_STALL_TIMEOUT_SECONDS has
    // elapsed since the last batch advanced).
    #[returns(Option<DistributionStateResponse>)]
    DistributionState {},
    // Creator-earnings rollup for dashboards: the claimable clip-slice
    // fee pot (emptied by `ExecuteMsg::ClaimCreatorFees`), the locked
    // excess-liquidity claim if one exists (with a `claimable_now` flag
    // computed against block time), and threshold-crossing context.
    // Everything here is already public state — this just saves
    // explorers a raw-storage crawl and gives the creator wallet one
    // call to render an earnings panel.
    #[returns(CreatorEarningsResponse)]
    CreatorEarnings {},
}

#[cw_serde]
pub struct CreatorEarningsResponse {
    /// Creator wallet configured at instantiation — the recipient of
    /// every claim path below.
    pub creator_wallet_address: Addr,
    /// Locked excess-liquidity claim created at threshold crossing when
    /// the seeded bluechip exceeded `max_bluechip_lock_per_pool`.
    /// `None` when no excess exists or it was already claimed.
    pub excess: Option<CreatorExcessEarningsResponse>,
    pub is_threshold_hit: bool,
    /// Block time at which the threshold flipped. `None` pre-threshold.
    pub threshold_crossed_at: Option<Timestamp>,
}

#[cw_serde]
pub struct CreatorExcessEarningsResponse {
    /// Raw bluechip earmarked for the creator (claimed as-is after unlock).
    pub bluechip_amount: Uint128,
    /// Raw creator tokens earmarked for the creator (claimed as-is after unlock).
    pub token_amount: Uint128,
    pub unlock_time: Timestamp,
    /// True once block time has reached `unlock_time`.
    pub claimable_now: bool,
}

#[cw_serde]
pub struct DistributionStateResponse {
    pub is_distributing: bool,
    pub distributions_remaining: u32,
    pub last_processed_key: Option<Addr>,
    pub started_at: Timestamp,
    pub last_updated: Timestamp,
    /// Block-time seconds since `last_updated` advanced. Computed at
    /// query time so dashboards don't have to do their own block-time
    /// math.
    pub seconds_since_update: u64,
    /// True when `seconds_since_update > DISTRIBUTION_STALL_TIMEOUT_SECONDS`.
    /// The on-chain handler (`process_distribution_batch`) will reject
    /// every keeper call with `"Distribution timeout - requires manual
    /// recovery"` while this flag is true; admin should call
    /// `RecoverPoolStuckStates::StuckDistribution` to reset the cursor.
    pub is_stalled: bool,
    pub consecutive_failures: u32,
    pub total_to_distribute: Uint128,
    pub total_committed_usd: Uint128,
    /// Running sum of creator-token rewards already minted across
    /// processed batches. Lets dashboards compute the residual dust
    /// (`total_to_distribute - distributed_so_far`) that will be
    /// settled to the creator wallet on the final batch.
    pub distributed_so_far: Uint128,
}

#[cw_serde]
pub struct FactoryNotifyStatusResponse {
    pub pending: bool,
}

/// Instantiate message dispatched by the factory to a freshly created pool
/// wasm.
///
/// Flat struct — this is the only pool wasm, so `instantiate` only ever
/// receives this shape. The factory sends it directly via
/// `WasmMsg::Instantiate { code_id: create_pool_wasm_contract_id, ... }`.
#[cw_serde]
pub struct PoolInstantiateMsg {
    pub pool_id: u64,
    /// The pool pair. Index 0 MUST be the bluechip `Native` side. Index 1
    /// is a `CreatorToken` PLACEHOLDER (its denom is ignored) — the pool
    /// creates its own TokenFactory denom at instantiate from `subdenom`
    /// and overwrites this slot with `CreatorToken { denom }`.
    pub pool_token_info: [TokenType; 2],
    pub used_factory_addr: Addr,
    pub threshold_payout: Option<Binary>,
    pub commit_fee_info: CommitFeeInfo,
    /// Commit threshold, USD-denominated (6 decimals).
    pub commit_threshold_limit_usd: Uint128,
    /// TokenFactory subdenom for the creator token. The pool creates
    /// `factory/{pool_contract_addr}/{subdenom}` at instantiate and
    /// becomes its denom admin. Replaces the old `token_address: Addr`
    /// (the CW20 contract) and `cw20_token_contract_id`.
    pub subdenom: String,
    pub max_bluechip_lock_per_pool: Uint128,
    pub creator_excess_liquidity_lock_days: u64,
}

#[cw_serde]
pub struct PoolCommitResponse {
    /// Number of `committers` entries in THIS page after filtering by
    /// `pool_contract_address` / `min_payment_usd` / `after_timestamp`
    /// and capping at `limit`. NOT a pre-filter total — paginating
    /// callers should treat `committers.len() < limit` as the
    /// end-of-data signal rather than relying on this field.
    pub page_count: u32,
    pub committers: Vec<CommitterInfo>,
}

#[cw_serde]
pub struct CommitterInfo {
    pub wallet: String,
    pub last_payment_bluechip: Uint128,
    pub last_payment_usd: Uint128,
    pub last_committed: Timestamp,
    pub total_paid_usd: Uint128,
    pub total_paid_bluechip: Uint128,
}

#[cw_serde]
pub struct LastCommittedResponse {
    pub has_committed: bool,
    pub last_committed: Option<Timestamp>,
    pub last_payment_bluechip: Option<Uint128>,
    pub last_payment_usd: Option<Uint128>,
}
