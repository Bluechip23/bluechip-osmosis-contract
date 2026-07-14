use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Binary, Uint128};

use cw20::{Cw20Coin, Logo, MinterResponse};

use crate::asset::TokenType;
use crate::pool_struct::{CommitFeeInfo, CreatePool, PoolConfigUpdate, RecoveryType};
use crate::state::FactoryInstantiate;

//triggers inside factory reply, used to complete the pool creation process.
#[cw_serde]
pub struct CreatePoolReplyMsg {
    pub pool_id: u64,
    /// Pool pair. Index 0 = bluechip `Native`. Index 1 = `CreatorToken`
    /// PLACEHOLDER — the pool creates its own TokenFactory denom from
    /// `subdenom` and overwrites the slot. Must be wire-compatible with
    /// the pool's `PoolInstantiateMsg`.
    pub pool_token_info: [TokenType; 2],
    pub used_factory_addr: Addr,
    //gets populated inside reply
    pub threshold_payout: Option<Binary>,
    //fees to bluechip and creator
    pub commit_fee_info: CommitFeeInfo,
    /// Commit threshold, USD-denominated (6 decimals).
    pub commit_threshold_limit_usd: Uint128,
    /// TokenFactory subdenom for the creator token. The pool creates
    /// `factory/{pool_addr}/{subdenom}` and becomes its denom admin.
    /// Replaces the old `token_address` (CW20 contract) and
    /// `cw20_token_contract_id`.
    pub subdenom: String,
    pub max_bluechip_lock_per_pool: Uint128,
    pub creator_excess_liquidity_lock_days: u64,
}

#[cw_serde]
pub enum ExecuteMsg {
    ProposeConfigUpdate {
        config: FactoryInstantiate,
    },
    UpdateConfig {},
    Create {
        pool_msg: CreatePool,
        token_info: CreatorTokenInfo,
    },
    UpgradePools {
        new_code_id: u64,
        pool_ids: Option<Vec<u64>>,
        migrate_msg: Binary,
    },
    CancelConfigUpdate {},
    ExecutePoolUpgrade {},
    CancelPoolUpgrade {},
    ContinuePoolUpgrade {},
    // 48-hour timelocked pool config changes.
    ProposePoolConfigUpdate {
        pool_id: u64,
        pool_config: PoolConfigUpdate,
    },
    ExecutePoolConfigUpdate {
        pool_id: u64,
    },
    CancelPoolConfigUpdate {
        pool_id: u64,
    },
    // Called by a pool contract when its commit threshold has been crossed.
    // Records the crossing in the factory registry (only fires once per pool).
    //
    // `crossed_at` is the pool's `env.block.time` at the moment threshold
    // flipped, recorded for observability. `serde(default)` keeps the
    // legacy wire shape working — None falls back to `env.block.time`.
    NotifyThresholdCrossed {
        pool_id: u64,
        #[serde(default)]
        crossed_at: Option<cosmwasm_std::Timestamp>,
    },

    // Admin-only pool admin forwards. The pool checks that info.sender ==
    // pool_info.factory_addr, so these must be routed through the factory
    // contract rather than called directly.
    PausePool {
        pool_id: u64,
    },
    UnpausePool {
        pool_id: u64,
    },
    // First call (no pending withdraw): initiates the 24h timelock and
    // pauses the pool. Second call (after the timelock): actually drains
    // pool reserves. The pool itself decides which phase based on state.
    EmergencyWithdrawPool {
        pool_id: u64,
    },
    CancelEmergencyWithdrawPool {
        pool_id: u64,
    },
    RecoverPoolStuckStates {
        pool_id: u64,
        recovery_type: RecoveryType,
    },

    // permissionless storage hygiene. Iterates the
    // per-address rate-limit map (commit-pool create) and removes entries older than 10× the cooldown window.
    // `batch_size` caps work per call so large maps don't exceed gas
    // limits; defaults to 100, hard-capped at 500. Anyone may call;
    // there is no bounty (the work is cheap and ops/keepers run it as
    // part of normal housekeeping).
    PruneRateLimits {
        batch_size: Option<u32>,
    },
}

#[cw_serde]
pub struct FactoryInstantiateResponse {
    pub factory: FactoryInstantiate,
}

/// Mirrors cw20-base's `InstantiateMarketingInfo`. Defined locally so the
/// factory doesn't need a cw20-base dependency just for one wire struct.
#[cw_serde]
pub struct InstantiateMarketingInfo {
    pub project: Option<String>,
    pub description: Option<String>,
    /// Address allowed to call `UpdateMarketing` / `UploadLogo` on the
    /// token after instantiation.
    pub marketing: Option<String>,
    pub logo: Option<Logo>,
}

#[cw_serde]
pub struct TokenInstantiateMsg {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub initial_balances: Vec<Cw20Coin>,
    pub mint: Option<MinterResponse>,
    /// cw20-base locks marketing forever when this is `None` at
    /// instantiation (`UpdateMarketing`/`UploadLogo` check the marketing
    /// admin, which can never be set later). The factory always passes
    /// `Some` with the pool creator as marketing admin so creators can
    /// attach a logo, description, and project URL to their token.
    pub marketing: Option<InstantiateMarketingInfo>,
}

#[cw_serde]
pub struct CreatorTokenInfo {
    pub name: String,
    pub symbol: String,
    pub decimal: u8,
}
