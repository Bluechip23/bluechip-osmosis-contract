use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Coin, Uint128};

pub mod asset;
pub mod cw721_msgs;
pub mod routing;

use crate::asset::TokenType;

#[cw_serde]
pub enum PoolQueryMsg {
    /// Returns this pool's `PoolStateResponseForFactory` (its own state — the
    /// pool is the implicit subject of the query). Takes no arguments by
    /// design: the dispatch always replies with the queried pool's own state,
    /// so a pool-address parameter would be dead weight and would invite
    /// readers to assume it selects which pool's state is returned.
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

    /// Everything a pool needs to process one commit, in a single query.
    /// The commit threshold is NATIVE-denominated (base units of the chain's
    /// native asset), so no price valuation happens here — a commit's value
    /// toward the threshold IS its attached native amount. The query supplies
    /// the live factory context a commit still needs: the current
    /// `bluechip_wallet_address` (so wallet rotations apply to every pool
    /// without a snapshot), the chain's GAMM pool-creation fee, and — when
    /// that fee is denominated in a non-native denom (osmosis-1 charges
    /// alloyed USDC) — the native budget for acquiring it, valued at the
    /// pricing pool's arithmetic TWAP. The TWAP is an on-chain chain-module
    /// read (no keeper, no external oracle); it bounds only the ~fee-sized
    /// swap at crossing, never the threshold itself.
    ///
    /// `include_fee_budget`: `Some(false)` skips the TWAP valuation —
    /// pools pass it on POST-threshold commits, which never fund a fee
    /// swap, so a pricing-pool TWAP outage cannot block trading-phase
    /// commits. Omitted/`None` means `true` (fail-safe: an older caller
    /// still gets the fail-closed budget).
    #[returns(CommitContextResponse)]
    CommitContext {
        #[serde(default)]
        include_fee_budget: Option<bool>,
    },

    /// Returns the factory's registered multi-hop router address, if any.
    /// Pools query this on a `SimpleSwap` that omits `belief_price`:
    /// direct callers must supply a `belief_price` (the on-chain estimate
    /// floor is not sandwich-resistant), but the registered router is
    /// exempt because it enforces an end-to-end `minimum_receive` across the
    /// whole route. `None` ⇒ no router registered, so EVERY null-belief
    /// SimpleSwap is rejected (fail-safe toward requiring the price bound).
    #[returns(RegisteredRouterResponse)]
    RegisteredRouter {},
}

/// Top-level envelope matching the factory's
/// `QueryMsg::PoolFactoryQuery(FactoryQueryMsg)` variant (wire key
/// `pool_factory_query`). The factory's root QueryMsg does NOT accept a
/// bare `FactoryQueryMsg` — pool-side callers MUST wrap their query in
/// this envelope or the factory fails to deserialize it.
///
/// Lives here (not in the factory crate) because pools intentionally
/// have no compile-time factory dependency; the two communicate only
/// over wasm message boundaries. Every pool-side factory query goes
/// through this one type so an unwrapped call can't slip in — a bare
/// `FactoryQueryMsg` fails the factory's deserialization, which would
/// hard-fail every emergency initiate on-chain (the
/// `EmergencyWithdrawDelaySeconds` caller in
/// `pool-core::execute_emergency_withdraw_initiate`), and would make
/// the three fail-soft `BluechipWalletAddress` callers silently fall
/// back to their instantiate-time snapshots forever, defeating live
/// wallet rotation.
#[cw_serde]
pub enum FactoryQueryEnvelope {
    PoolFactoryQuery(FactoryQueryMsg),
}

#[cw_serde]
pub struct EmergencyWithdrawDelayResponse {
    pub delay_seconds: u64,
}

#[cw_serde]
pub struct BluechipWalletResponse {
    pub address: Addr,
}

/// Response to `RegisteredRouter`. `router` is `None` when no router
/// has been registered on the factory yet.
#[cw_serde]
pub struct RegisteredRouterResponse {
    pub router: Option<Addr>,
}

/// Response to `CommitContext`. All fields are LIVE factory state (the
/// admin-tunable values ride the query every commit already makes, so pools
/// never act on an instantiate-time snapshot).
#[cw_serde]
pub struct CommitContextResponse {
    /// Block time the context was assembled at (the current block).
    pub timestamp: u64,
    /// The factory's current `bluechip_wallet_address` — protocol-fee
    /// recipient and threshold-cross reward target.
    pub bluechip_wallet: Addr,
    /// Factory's configured `gamm_pool_creation_fee` (denom + amount) — the
    /// fee `x/poolmanager` auto-charges when the crossing creates the native
    /// GAMM pool. `None` ⇒ fee collection disabled.
    #[serde(default)]
    pub gamm_pool_creation_fee: Option<Coin>,
    /// Native base units budgeted to ACQUIRE `gamm_pool_creation_fee` via the
    /// pricing pool when the fee denom is non-native: `fee.amount` valued at
    /// the pricing pool's arithmetic TWAP over the factory's trailing window,
    /// WITHOUT margin (the pool applies its own swap margin on top). `None`
    /// when the fee is native-denominated (pay directly, no swap) or fee
    /// collection is disabled. Fail-closed: if the TWAP read errors the whole
    /// query errors and the commit reverts rather than mis-budgeting.
    #[serde(default)]
    pub fee_swap_budget_native: Option<Uint128>,
    /// Factory's cross-denom fee-swap pool (trades the native denom against
    /// `fee_quote_denom` to acquire the gamm creation fee at crossing; NOT a
    /// price source for the threshold). `0` ⇒ unused (native-denominated fee).
    #[serde(default)]
    pub pricing_pool_id: u64,
    /// The non-native quote denom on the pricing pool (the denom the gamm
    /// creation fee is charged in on chains where it is non-native). Empty ⇒
    /// unused.
    #[serde(default)]
    pub fee_quote_denom: String,
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
    // `#[serde(default)]` keeps the wire format tolerant: callers that
    // omit the field deserialize with `crossed_at = None`, and the
    // factory falls back to `env.block.time`. Production callers in
    // this workspace always supply the field.
    NotifyThresholdCrossed {
        pool_id: u64,
        #[serde(default)]
        crossed_at: Option<cosmwasm_std::Timestamp>,
    },
}
