//! Shared state — every storage Item, struct, and constant that both
//! pool kinds read or write.
//!
//! Phase-2 note: the pool no longer runs an INTERNAL constant-product AMM.
//! At threshold-crossing it seeds a native Osmosis GAMM balancer pool and
//! holds the `gamm/pool/{id}` LP shares permanently. Consequently the old
//! reserve/liquidity-position/fee-growth machinery is gone: there are no
//! `reserve0/reserve1`, no LP positions, no internal fee accounting.
//! `POOL_STATE` shrinks to the pool's own address, and the native pool id
//! learned from the `MsgCreateBalancerPool` reply lives in `POOL_ID`.
//!
//! The creator-pool crate glob-re-exports this module from its own
//! `state.rs` so existing `use crate::state::X;` call sites keep
//! resolving unchanged.

use crate::msg::CommitFeeInfo;
use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Decimal, StdResult, Storage, Timestamp, Uint128};
use cw_storage_plus::{Item, Map};
use pool_factory_interfaces::asset::{PoolPairType, TokenInfo, TokenType};

// -- Structs --------------------------------------------------------------

#[cw_serde]
pub struct TokenMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[cw_serde]
pub struct PoolAnalytics {
    /// Total number of swaps executed on this pool (native-pool swaps
    /// routed through `MsgSwapExactAmountIn`, plus post-threshold commits).
    pub total_swap_count: u64,
    /// Total number of commits (pre- and post-threshold).
    pub total_commit_count: u64,
    /// Cumulative volume of token0 (bluechip) that flowed through swaps.
    pub total_volume_0: Uint128,
    /// Cumulative volume of token1 (creator token) that flowed through swaps.
    pub total_volume_1: Uint128,
    /// Retained for wire compatibility. The internal LP system is gone —
    /// third-party liquidity now lives directly on the native Osmosis
    /// pool — so these counters stay at zero.
    pub total_lp_deposit_count: u64,
    pub total_lp_withdrawal_count: u64,
    /// Block height of the last trade (swap or post-threshold commit).
    pub last_trade_block: u64,
    /// Block timestamp of the last trade.
    pub last_trade_timestamp: u64,
}

impl Default for PoolAnalytics {
    fn default() -> Self {
        Self {
            total_swap_count: 0,
            total_commit_count: 0,
            total_volume_0: Uint128::zero(),
            total_volume_1: Uint128::zero(),
            total_lp_deposit_count: 0,
            total_lp_withdrawal_count: 0,
            last_trade_block: 0,
            last_trade_timestamp: 0,
        }
    }
}

/// Record written on completed emergency drain (Phase 2). Kept for the
/// simplified native-pool emergency-withdraw path; `amount0/amount1`
/// capture whatever the drain swept to the bluechip wallet.
#[cw_serde]
pub struct EmergencyWithdrawalInfo {
    pub withdrawn_at: u64,
    pub recipient: Addr,
    pub amount0: Uint128,
    pub amount1: Uint128,
    pub total_liquidity_at_withdrawal: Uint128,
}

/// Mutable pool state.
///
/// Phase-2: shrunk to the pool's own contract address. The internal AMM's
/// reserves, cumulative-price accumulators, and total-liquidity counter
/// are gone — pricing and depth live on the native Osmosis pool now, keyed
/// by [`POOL_ID`].
#[cw_serde]
pub struct PoolState {
    pub pool_contract_address: Addr,
}

/// Instantiate-time self-check storage. The pool's instantiate saves
/// the caller-declared `msg.used_factory_addr` here and verifies
/// `info.sender == expected_factory_address` to reject direct-instantiate
/// attempts that don't go through the factory's reply chain.
///
/// **NOT the canonical auth source post-instantiate.** Every admin-gated
/// handler that runs after instantiate must check against
/// `POOL_INFO.factory_addr` instead.
#[cw_serde]
pub struct ExpectedFactory {
    pub expected_factory_address: Addr,
}

#[cw_serde]
pub struct PoolSpecs {
    /// LP fee. Reused as the native GAMM pool's `swap_fee` when the pool
    /// is seeded at threshold-crossing (`create_balancer_pool_msg`).
    pub lp_fee: Decimal,
    pub min_commit_interval: u64,
}

#[cw_serde]
pub struct PoolInfo {
    pub pool_id: u64,
    pub pool_info: PoolDetails,
    pub factory_addr: Addr,
    /// The creator token's native TokenFactory bank denom
    /// (`factory/{pool_addr}/{subdenom}`). The pool contract is the denom
    /// admin, so it mints (threshold payout / distribution) and transfers
    /// this denom via bank messages.
    pub token_denom: String,
}

#[cw_serde]
pub struct PoolDetails {
    pub asset_infos: [TokenType; 2],
    pub contract_addr: Addr,
    pub pool_type: PoolPairType,
}

impl PoolDetails {
    pub fn query_pools(
        &self,
        querier: &cosmwasm_std::QuerierWrapper,
        contract_addr: Addr,
    ) -> StdResult<[TokenInfo; 2]> {
        pool_factory_interfaces::asset::query_pools(&self.asset_infos, querier, contract_addr)
    }
}

/// Core state items read by the swap / commit hot paths. Bundled so
/// handlers that touch more than one can `load` once.
///
/// Phase-2: `fees` is gone (no internal fee accounting) and `state` no
/// longer carries reserves — swaps route through the native pool.
pub struct PoolCtx {
    pub info: PoolInfo,
    pub state: PoolState,
    pub specs: PoolSpecs,
}

impl PoolCtx {
    pub fn load(storage: &dyn Storage) -> StdResult<Self> {
        Ok(Self {
            info: POOL_INFO.load(storage)?,
            state: POOL_STATE.load(storage)?,
            specs: POOL_SPECS.load(storage)?,
        })
    }
}

// -- Storage Items & Maps -------------------------------------------------

/// Pool identity and addresses (factory, token).
pub const POOL_INFO: Item<PoolInfo> = Item::new("pool_info");
/// Mutable pool state (just the pool's own address, post Phase-2).
pub const POOL_STATE: Item<PoolState> = Item::new("pool_state");
/// The native Osmosis GAMM pool id, learned from the
/// `MsgCreateBalancerPool` reply at threshold-crossing. Unset until then.
pub const POOL_ID: Item<u64> = Item::new("gamm_pool_id");
/// Tunable pool parameters (lp_fee used as the gamm swap_fee, min_commit_interval).
pub const POOL_SPECS: Item<PoolSpecs> = Item::new("pool_specs");
/// Cumulative counters for swaps, commits.
pub const POOL_ANALYTICS: Item<PoolAnalytics> = Item::new("pool_analytics");
/// Top-level pause flag — true if the pool is paused for any reason.
pub const POOL_PAUSED: Item<bool> = Item::new("pool_paused");
/// Distinguishes admin/emergency pause (false) from auto-pause (true).
/// Retained for wire/behaviour compatibility; auto-pause-on-low-liquidity
/// no longer fires (no internal reserves), so this is effectively always
/// false in Phase-2.
pub const POOL_PAUSED_AUTO: Item<bool> = Item::new("pool_paused_auto");
/// Record written on completed emergency drain (Phase 2 drain).
pub const EMERGENCY_WITHDRAWAL: Item<EmergencyWithdrawalInfo> = Item::new("emergency_withdrawal");
/// Effective-after timestamp armed by Phase 1 (initiate); cleared by
/// Phase 2 (drain) or by cancel.
pub const PENDING_EMERGENCY_WITHDRAW: Item<Timestamp> = Item::new("pending_emergency_withdraw");
/// Permanent flag set after a successful emergency drain.
pub const EMERGENCY_DRAINED: Item<bool> = Item::new("emergency_drained");
/// Expected factory address pinned at instantiate for sanity checks.
pub const EXPECTED_FACTORY: Item<ExpectedFactory> = Item::new("expected_factory");

// Reentrancy lock acquired by `commit` and `simple_swap` to reject
// re-entry within the same tx. The storage key string must remain
// `"rate_limit_guard"` because already-deployed pools persist the lock
// under that key. Despite the key's name this is a reentrancy guard, not
// a rate limiter — rate limiting is handled by USER_LAST_COMMIT.
pub const REENTRANCY_LOCK: Item<bool> = Item::new("rate_limit_guard");

/// Per-user timestamp of last commit, used by swap/commit rate limiting.
pub const USER_LAST_COMMIT: Map<&Addr, u64> = Map::new("user_last_commit");

/// Liquidity-operation rate-limit cooldown, kept in a SEPARATE map from
/// `USER_LAST_COMMIT`. Retained for compatibility with the shared
/// rate-limit helper; the internal LP paths that stamped it are gone.
pub const USER_LAST_LIQUIDITY_OP: Map<&Addr, u64> = Map::new("user_last_liquidity_op");

/// Written `false` at pool instantiate; flipped `true` by the
/// threshold-crossing commit path.
pub const IS_THRESHOLD_HIT: Item<bool> = Item::new("threshold_hit");

/// Per-side liquidity actually seeded into the native GAMM pool at
/// threshold-crossing, snapshotted as `(seed_osmo, seed_creator)` — the
/// exact `(bluechip, creator)` amounts passed to `MsgCreateBalancerPool`
/// AFTER the FIX-E creation-fee adjustment. This is the reference point
/// for the FIX-G relative circuit breaker: a swap is halted if EITHER
/// side of the live native pool has fallen below
/// `BREAKER_FLOOR_PERCENT`% of its seeded amount here. Unset until the
/// pool crosses its threshold (no native pool exists before then).
pub const SEED_LIQUIDITY: Item<(Uint128, Uint128)> = Item::new("seed_liquidity");

/// emergency_withdraw reads `bluechip_wallet_address` for the drain
/// recipient.
pub const COMMITFEEINFO: Item<CommitFeeInfo> = Item::new("fee_info");

// -- Reply IDs ------------------------------------------------------------
//
// Kept in pool-core (rather than creator-pool) because the swap
// orchestration in `pool_core::swap` needs `REPLY_ID_SWAP_FORWARD`, and
// pool-core cannot depend on the creator-pool crate. Both are re-exported
// through the creator-pool `state` glob so its `reply` dispatch and
// threshold-payout code reach them at the usual `crate::state::` path.
// Distinct from the creator-pool factory-notify ids (1, 2) and the
// distribution-mint base (1_000_000).

/// Reply id for the `MsgCreateBalancerPool` SubMsg emitted at
/// threshold-crossing. The reply parses `MsgCreateBalancerPoolResponse`
/// and stores the resulting `pool_id` in [`POOL_ID`].
pub const REPLY_ID_CREATE_POOL: u64 = 3;

/// Reply id for the `MsgSwapExactAmountIn` SubMsg emitted by a swap /
/// post-threshold commit. The reply parses `MsgSwapExactAmountInResponse`
/// and `BankMsg::Send`s `token_out_amount` to the receiver carried in the
/// SubMsg `payload` (a JSON-encoded [`SwapForwardPayload`]).
pub const REPLY_ID_SWAP_FORWARD: u64 = 4;

/// Payload carried on the `REPLY_ID_SWAP_FORWARD` SubMsg so the reply
/// handler knows where to forward the swapped-out tokens.
#[cw_serde]
pub struct SwapForwardPayload {
    /// Recipient of the swapped-out tokens.
    pub receiver: Addr,
    /// Denom of the swapped-out tokens (the ask denom).
    pub token_out_denom: String,
    /// Original sender of the swap (for response attributes).
    pub sender: Addr,
    /// The offer amount that was swapped in (for response attributes).
    pub offer_amount: Uint128,
    /// The offer denom that was swapped in (for response attributes).
    pub offer_denom: String,
}

// -- Constants ------------------------------------------------------------

/// Default per-commit rate-limit floor (seconds).
pub const DEFAULT_SWAP_RATE_LIMIT_SECS: u64 = 13;

/// Default LP fee charged on every swap. Reused as the native GAMM
/// pool's swap_fee at seeding time. 30 bps = 0.3%.
pub const DEFAULT_LP_FEE: Decimal = Decimal::permille(3);
/// Hard ceiling on `PoolSpecs.lp_fee`. 10%.
pub const MAX_LP_FEE: Decimal = Decimal::percent(10);
/// Hard floor on `PoolSpecs.lp_fee`. 0.1%.
pub const MIN_LP_FEE: Decimal = Decimal::permille(1);

/// `pool_kind` attribute value emitted in `instantiate` responses.
pub const POOL_KIND_COMMIT: &str = "commit";

/// FIX-G relative circuit-breaker floor, as a whole-number percent of the
/// seeded per-side liquidity ([`SEED_LIQUIDITY`]). If EITHER side of the
/// live native pool drops below this percentage of what was seeded, the
/// next routed swap trips the breaker: it sets `POOL_PAUSED` +
/// `POOL_PAUSED_AUTO` and rejects with a low-liquidity pause error. This
/// replaces the retired absolute `MINIMUM_LIQUIDITY` guard, which is
/// meaningless once reserves live on the native pool rather than in local
/// state. Manual admin `Unpause` clears both pause flags as it does today.
pub const BREAKER_FLOOR_PERCENT: u128 = 25;

/// Recovery window for `RecoverPoolStuckStates::StuckThreshold`.
pub const STUCK_THRESHOLD_RECOVERY_WINDOW_SECONDS: u64 = 3_600;

/// Recovery window for `recover_distribution`.
pub const STUCK_DISTRIBUTION_RECOVERY_WINDOW_SECONDS: u64 = 3_600;

/// Hard cap on consecutive distribution failures before the batch
/// processor halts the cursor.
pub const MAX_CONSECUTIVE_DISTRIBUTION_FAILURES: u32 = 5;

/// Seconds in a day. Used by the creator-excess unlock-time math.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Default page size for `QueryMsg::PoolCommits`.
pub const POOL_COMMITS_QUERY_DEFAULT_LIMIT: u32 = 30;
/// Hard ceiling on `QueryMsg::PoolCommits.limit`.
pub const POOL_COMMITS_QUERY_MAX_LIMIT: u32 = 100;

/// Threshold-payout split components (bluechip base units).
pub const THRESHOLD_PAYOUT_CREATOR_BASE_UNITS: u128 = 325_000_000_000;
pub const THRESHOLD_PAYOUT_BLUECHIP_BASE_UNITS: u128 = 25_000_000_000;
pub const THRESHOLD_PAYOUT_POOL_BASE_UNITS: u128 = 350_000_000_000;
pub const THRESHOLD_PAYOUT_COMMIT_RETURN_BASE_UNITS: u128 = 500_000_000_000;
pub const THRESHOLD_PAYOUT_TOTAL_BASE_UNITS: u128 = 1_200_000_000_000;

/// Classify the pool's current pause state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKind {
    /// Pool is open. POOL_PAUSED == false.
    None,
    /// Reserves fell below a floor after a swap or remove. Retained for
    /// completeness; not armed in Phase-2 (no internal reserves).
    AutoLowLiquidity,
    /// Phase-1 emergency-withdraw is armed and inside the timelock.
    EmergencyPending,
    /// Explicit admin Pause (or any other non-emergency hard pause).
    Hard,
}

/// Resolve POOL_PAUSED + POOL_PAUSED_AUTO + PENDING_EMERGENCY_WITHDRAW
/// into a `PauseKind`. Reads only — does not mutate.
pub fn pause_kind(storage: &dyn Storage) -> StdResult<PauseKind> {
    if !POOL_PAUSED.may_load(storage)?.unwrap_or(false) {
        return Ok(PauseKind::None);
    }
    if POOL_PAUSED_AUTO.may_load(storage)?.unwrap_or(false) {
        return Ok(PauseKind::AutoLowLiquidity);
    }
    if PENDING_EMERGENCY_WITHDRAW.may_load(storage)?.is_some() {
        return Ok(PauseKind::EmergencyPending);
    }
    Ok(PauseKind::Hard)
}
