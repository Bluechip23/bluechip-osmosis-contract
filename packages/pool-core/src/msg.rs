//! Shared wire-format types for consuming pool contract crates.
//!
//! Split boundary:
//! - Shared (this module): CommitFeeInfo, PoolConfigUpdate, Cw20HookMsg,
//! CommitStatus, and every response struct returned by a query
//! handler that lives in `pool_core::query`.
//! - Per-contract (in creator-pool): ExecuteMsg,
//! QueryMsg, MigrateMsg, PoolInstantiateMsg,
//! and commit-only response types (FactoryNotifyStatusResponse,
//! PoolCommitResponse, CommitterInfo, LastCommittedResponse).
//!
//! Wire format is load-bearing — every struct keeps its `#[cw_serde]`
//! attribute, and JSON shapes (field names, nested layouts) must stay
//! byte-for-byte compatible with what deployed pools and clients emit.

use crate::asset::TokenInfo;
use crate::state::PoolAnalytics;
use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Binary, Decimal, Timestamp, Uint128};

#[cw_serde]
pub struct CommitFeeInfo {
    pub bluechip_wallet_address: Addr,
    pub creator_wallet_address: Addr,
    pub commit_fee_bluechip: Decimal,
    pub commit_fee_creator: Decimal,
}

#[cw_serde]
#[derive(Default)]
pub struct PoolConfigUpdate {
    pub lp_fee: Option<Decimal>,
    pub min_commit_interval: Option<u64>,
    /// Per-pool override for the pre-threshold minimum commit value
    /// (in USD, 6 decimals). When `Some(_)` the pool
    /// updates `CommitLimitInfo.min_commit_usd_pre_threshold` to the
    /// new value; `None` leaves it unchanged. Bounds enforced by the
    /// factory at propose time and re-enforced by the pool's wrapper
    /// dispatch on apply.
    ///
    /// `#[serde(default)]` keeps records / clients written before this
    /// field existed wire-compatible (the field deserializes as `None`
    /// when absent).
    #[serde(default)]
    pub min_commit_usd_pre_threshold: Option<Uint128>,
    /// Per-pool override for the post-threshold minimum commit value
    /// (in USD, 6 decimals). Same shape and rules as
    /// `min_commit_usd_pre_threshold` above.
    #[serde(default)]
    pub min_commit_usd_post_threshold: Option<Uint128>,
    // There is deliberately no per-pool price-source knob. One would
    // be an admin-compromise vector: a malicious source can return
    // arbitrary `ConversionResponse.amount`, letting a $5 commit register
    // as a $25k threshold-cross and capturing the full pool seed +
    // creator rewards on a single pool. USD pricing is pinned to the
    // factory (`factory::usd_price`); re-routing, if ever needed, is a
    // coordinated wasm migration via `UpgradePools` (already
    // 48h-timelocked + batched), not a per-pool config knob.
}

#[cw_serde]
pub enum Cw20HookMsg {
    Swap {
        belief_price: Option<Decimal>,
        max_spread: Option<Decimal>,
        #[serde(default)]
        allow_high_max_spread: Option<bool>,
        to: Option<String>,
        transaction_deadline: Option<Timestamp>,
    },
}

#[cw_serde]
pub enum CommitStatus {
    InProgress { raised: Uint128, target: Uint128 },
    FullyCommitted,
}

#[cw_serde]
pub struct PoolResponse {
    pub assets: [TokenInfo; 2],
}

#[cw_serde]
pub struct ConfigResponse {
    pub block_time_last: u64,
    pub params: Option<Binary>,
}

#[cw_serde]
pub struct SimulationResponse {
    pub return_amount: Uint128,
    pub spread_amount: Uint128,
    pub commission_amount: Uint128,
}

#[cw_serde]
pub struct FeeInfoResponse {
    pub fee_info: CommitFeeInfo,
}

#[cw_serde]
pub struct PoolStateResponse {
    pub nft_ownership_accepted: bool,
    pub reserve0: Uint128,
    pub reserve1: Uint128,
    pub total_liquidity: Uint128,
    pub block_time_last: u64,
}

#[cw_serde]
pub struct PoolFeeStateResponse {
    pub fee_growth_global_0: Decimal,
    pub fee_growth_global_1: Decimal,
    pub total_fees_collected_0: Uint128,
    pub total_fees_collected_1: Uint128,
}

#[cw_serde]
pub struct PoolInfoResponse {
    pub pool_state: PoolStateResponse,
    pub fee_state: PoolFeeStateResponse,
    pub total_positions: u64,
}

#[cw_serde]
pub struct PoolAnalyticsResponse {
    pub analytics: PoolAnalytics,
    pub current_price_0_to_1: String,
    pub current_price_1_to_0: String,
    pub total_value_locked_0: Uint128,
    pub total_value_locked_1: Uint128,
    pub fee_reserve_0: Uint128,
    pub fee_reserve_1: Uint128,
    pub threshold_status: CommitStatus,
    pub total_usd_raised: Uint128,
    pub total_bluechip_raised: Uint128,
    pub total_positions: u64,
}
