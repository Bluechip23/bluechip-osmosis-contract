use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Uint128};

pub mod asset;
pub mod cw721_msgs;
pub mod routing;

use crate::asset::TokenType;

#[cw_serde]
pub enum PoolQueryMsg {
    /// Returns this pool's `PoolStateResponseForFactory` (its own state — the
    /// pool is the implicit subject of the query). Previously took a
    /// `pool_contract_address: String` argument that was never read by any
    /// implementor; the dispatch always replied with the queried pool's own
    /// state. Removed to prevent future readers from assuming the parameter
    /// changed which pool's state was returned.
    GetPoolState {},
    GetAllPools {},
    IsPaused {},
}

#[cw_serde]
pub struct IsPausedResponse {
    pub paused: bool,
}

/// Registry-membership + canonical-pair record for a pool *contract
/// address*, returned by the factory's `PoolByAddress` query. The factory
/// returns `Some(..)` only for an address it created and registered, and
/// `None` for any other address.
///
/// Consumed by the router to validate caller-supplied hop `pool_addr`s
/// against the factory's authoritative registry before routing user funds
/// through them. Without this, the router would forward funds to whatever
/// contract address a (possibly malicious) frontend supplied, with
/// `minimum_receive` as the only guard. `pool_token_info` lets the caller
/// additionally confirm the hop's declared (offer, ask) are the pool's two
/// real sides.
#[cw_serde]
pub struct RegisteredPoolResponse {
    pub pool_id: u64,
    pub pool_token_info: [TokenType; 2],
}
#[cw_serde]
#[derive(QueryResponses)]
pub enum FactoryQueryMsg {
    /// Values `amount` (base units of the chain's native asset, e.g.
    /// uosmo) in USD micro-units (6 decimals). Backed by Osmosis's
    /// x/twap module over the factory-configured native/USD-stable pool
    /// — chain-native pricing, no keeper or external oracle. Pools call
    /// this once per commit to value the deposit against the
    /// USD-denominated threshold. Fails (and therefore the commit fails
    /// closed) if the TWAP query errors.
    #[returns(ConversionResponse)]
    ConvertNativeToUsd { amount: Uint128 },

    /// Returns the chain-side emergency-withdraw delay (seconds between
    /// `Phase 1: initiate` and `Phase 2: drain` on each pool's
    /// `EmergencyWithdraw` flow). Pools query this at initiate time so
    /// the value tracks `factory_config.emergency_withdraw_delay_seconds`,
    /// which is admin-tunable through the standard 48h
    /// `ProposeConfigUpdate` flow.
    #[returns(EmergencyWithdrawDelayResponse)]
    EmergencyWithdrawDelaySeconds {},

    /// Returns the factory's current `bluechip_wallet_address`. Pools
    /// query this at emergency-drain Phase 2 to route the swept funds
    /// to the live wallet rather than a stale snapshot taken at pool
    /// instantiate time. The address is admin-tunable through the
    /// standard 48h `ProposeConfigUpdate` flow; a snapshot would leave
    /// existing pools draining to whatever wallet the admin had
    /// configured when each pool was created, which would either
    /// scatter drain proceeds across multiple historical wallets or
    /// (worse) route them to a wallet the admin has since rotated
    /// away from.
    #[returns(BluechipWalletResponse)]
    BluechipWalletAddress {},
}

#[cw_serde]
pub struct EmergencyWithdrawDelayResponse {
    pub delay_seconds: u64,
}

#[cw_serde]
pub struct BluechipWalletResponse {
    pub address: Addr,
}

/// Result of a native→USD valuation. `rate_used` is the price in
/// micro-USD per micro-native (6-decimal fixed point: `1_000_000` means
/// $1.00 per native token), so callers can convert back
/// (`native = usd * 1_000_000 / rate_used`) at EXACTLY the rate the
/// valuation used — no mid-tx drift. `timestamp` is the block time the
/// TWAP was computed at (always the current block for x/twap-to-now).
#[cw_serde]
pub struct ConversionResponse {
    pub amount: Uint128,
    pub rate_used: Uint128,
    pub timestamp: u64,
}

#[cw_serde]
pub struct PoolStateResponseForFactory {
    pub pool_contract_address: Addr,
    pub nft_ownership_accepted: bool,
    pub reserve0: Uint128,
    pub reserve1: Uint128,
    pub total_liquidity: Uint128,
    pub block_time_last: u64,
    pub price0_cumulative_last: Uint128,
    pub price1_cumulative_last: Uint128,
    pub assets: Vec<String>,
}

#[cw_serde]
pub struct AllPoolsResponse {
    pub pools: Vec<(String, PoolStateResponseForFactory)>,
}

// Messages that a pool contract can send to the factory contract.
#[cw_serde]
pub enum FactoryExecuteMsg {
    // Called by a pool when its commit threshold has been crossed.
    //
    // `crossed_at` is the pool's `env.block.time` at the moment the
    // threshold flipped (snapshotted by `trigger_threshold_payout` into
    // pool storage). Recorded by the factory for observability so the
    // registry reflects when the pool ACTUALLY crossed — not when
    // the (possibly retried-after-failure) notification finally lands.
    //
    // `#[serde(default)]` keeps the wire format backward-compatible:
    // legacy callers (no field) deserialize with `crossed_at = None`,
    // and the factory falls back to `env.block.time` (the prior
    // behaviour). Production callers in this workspace always supply
    // the field.
    NotifyThresholdCrossed {
        pool_id: u64,
        #[serde(default)]
        crossed_at: Option<cosmwasm_std::Timestamp>,
    },
}
