//! External message surface for the router contract.
//!
//! The router exposes:
//! - `InstantiateMsg` for setup
//! - `ExecuteMsg::ExecuteMultiHop` for all routes (the user attaches the
//! first-hop offer as funds). Both the bluechip side and the creator
//! TokenFactory denom are native bank coins, so there is a single
//! native entry point — the old CW20 `Receive` path was removed with
//! the creator-token migration.
//! - `ExecuteMsg::UpdateConfig` for admin rotation
//! - Two internal variants (`ExecuteSwapOperation`, `AssertReceived`)
//! that the router invokes on itself; both reject any caller other
//! than the router contract address
//! - `QueryMsg::SimulateMultiHop` for pre-trade UX
//! - `QueryMsg::Config` for config reads

use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Decimal, Timestamp, Uint128};
use pool_factory_interfaces::asset::TokenType;
use pool_factory_interfaces::routing::SwapOperation;

/// Parameters for instantiating the router. The bluechip denom and
/// factory address pin the router to a single Bluechip deployment;
/// admin can be rotated later via [`ExecuteMsg::UpdateConfig`].
#[cw_serde]
pub struct InstantiateMsg {
    pub factory_addr: String,
    pub bluechip_denom: String,
    pub admin: String,
}

/// Mutating entry points.
///
/// Note: end-to-end slippage is enforced via `minimum_receive` on the
/// final ask token, NOT via per-hop `belief_price` / `max_spread`. A
/// single per-pair belief price is meaningless across hops on heterogeneous
/// pairs (units differ between `A/bluechip` and `bluechip/B`), so the
/// router does not accept those parameters. On the underlying pool calls
/// it passes `belief_price = None` and pins `max_spread` to the pools'
/// 5% hard cap — passing `None` would make pools substitute their 0.5%
/// default and silently gate every thin-pool hop regardless of the
/// caller's tolerance. Frontends should size `minimum_receive` from the
/// simulation result (see `SimulateMultiHop`).
#[cw_serde]
pub enum ExecuteMsg {
    /// Run a multi-hop swap whose first hop offers the native bluechip
    /// denom. The caller attaches the offer amount via `info.funds`.
    /// The router does not perform on-chain pathfinding -- the caller
    /// supplies the entire route.
    ExecuteMultiHop {
        operations: Vec<SwapOperation>,
        minimum_receive: Uint128,
        deadline: Option<Timestamp>,
        recipient: Option<String>,
    },
    /// Admin-only. Step 1 of a 48h-timelocked config change. Records a
    /// `PendingConfigUpdate` with `effective_after = now + 48h`. Either
    /// field may be `None` to leave that field unchanged. Re-proposing
    /// while a pending proposal exists is rejected with
    /// `ConfigUpdateAlreadyPending` — the admin must `CancelConfigUpdate`
    /// first, so any community watcher polling `PENDING_CONFIG` sees an
    /// explicit cancellation event before a replacement proposal lands.
    ProposeConfigUpdate {
        admin: Option<String>,
        factory_addr: Option<String>,
    },
    /// Admin-only. Step 2 of the timelocked flow. Applies the pending
    /// proposal once `effective_after` has elapsed. Errors with
    /// `NoPendingConfigUpdate` if no proposal is pending or
    /// `TimelockNotExpired` if invoked too early.
    UpdateConfig {},
    /// Admin-only. Cancels a pending proposal before it can be applied.
    CancelConfigUpdate {},
    /// Internal: invoked by the router on itself once per hop. Each
    /// handler dispatches the underlying pool swap. Rejected unless the
    /// caller is the router contract.
    ///
    /// The swap input is `current_offer_balance - offer_baseline`, where
    /// `offer_baseline` is the router's PRE-route balance of the offer
    /// denom (snapshotted at route start, and reduced by the attached
    /// first-hop offer amount for the input denom). This ensures each hop
    /// swaps only the funds THIS route produced — the attached input on
    /// hop 0 and the prior hop's output on later hops — never a pre-existing
    /// or donated balance the router happened to hold (M-03).
    ExecuteSwapOperation {
        operation: SwapOperation,
        hop_index: u32,
        to: String,
        #[serde(default)]
        offer_baseline: Uint128,
    },
    /// Internal: final slippage assertion. Compares the recipient's
    /// post-route balance against the captured pre-route balance plus
    /// the minimum-receive threshold. Rejected unless the caller is the
    /// router contract.
    AssertReceived {
        ask_info: TokenType,
        recipient: String,
        prev_balance: Uint128,
        minimum_receive: Uint128,
    },
}

/// Read-only entry points.
#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    /// Pre-trade simulation that mirrors the execution path. Lets a
    /// frontend show the expected final amount, every intermediate
    /// amount, and a coarse price-impact estimate before signing.
    #[returns(SimulateMultiHopResponse)]
    SimulateMultiHop {
        operations: Vec<SwapOperation>,
        offer_amount: Uint128,
    },
    /// Returns the current router configuration.
    #[returns(ConfigResponse)]
    Config {},
}

/// Response for [`QueryMsg::Config`].
#[cw_serde]
pub struct ConfigResponse {
    pub factory_addr: Addr,
    pub bluechip_denom: String,
    pub admin: Addr,
}

/// Response for [`QueryMsg::SimulateMultiHop`].
///
/// `intermediate_amounts` contains the *output* of every hop in order,
/// so `intermediate_amounts.last()` always equals `final_amount`.
#[cw_serde]
pub struct SimulateMultiHopResponse {
    pub final_amount: Uint128,
    pub intermediate_amounts: Vec<Uint128>,
    pub price_impact: Decimal,
}
