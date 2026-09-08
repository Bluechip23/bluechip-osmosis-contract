//! Swap-math re-exports plus the commit-context client.
//!
//! The pure AMM math (`compute_swap`, `compute_offer_amount`,
//! `assert_max_spread`, `update_price_accumulator`, `DEFAULT_SLIPPAGE`)
//! lives in `pool_core::swap` and is re-exported below so imports like
//! `use crate::swap_helper::compute_swap;` resolve here.
pub use pool_core::swap::*;

use cosmwasm_std::{Addr, Deps, StdResult};
use pool_factory_interfaces::{
    CommitContextResponse, FactoryQueryEnvelope, FactoryQueryMsg, RegisteredRouterResponse,
};

/// Fetch the live factory context a commit needs, in one cross-contract
/// round-trip: the current `bluechip_wallet_address` (protocol-fee recipient
/// + threshold-cross reward target — live so an admin wallet rotation takes
/// effect for every existing pool without a snapshot), the chain's GAMM
/// creation-fee coin, and — when that fee is non-native — the TWAP-valued
/// native budget for acquiring it at crossing.
///
/// The commit itself is never valued: the threshold is denominated in the
/// native asset, so a commit's value toward it is simply its attached
/// amount. The query DOES read a price when `include_fee_budget` is true —
/// the factory's fee-route TWAP that budgets the GAMM creation-fee swap —
/// and it is fail-closed: any error (factory unreachable, TWAP query
/// failure, implausible price) propagates and reverts the commit.
pub fn get_commit_context(
    deps: Deps,
    factory_addr: &Addr,
    include_fee_budget: bool,
) -> StdResult<CommitContextResponse> {
    deps.querier.query_wasm_smart(
        factory_addr.to_string(),
        &FactoryQueryEnvelope::PoolFactoryQuery(FactoryQueryMsg::CommitContext {
            // `false` on post-threshold commits: they never fund a fee
            // swap, so skipping the factory's TWAP valuation keeps
            // trading-phase commits alive through a pricing-pool outage.
            include_fee_budget: Some(include_fee_budget),
        }),
    )
}

/// Query the factory for its registered multi-hop router address.
/// Used by `SimpleSwap` to decide whether a caller that supplied no
/// `belief_price` is the exempt router (which enforces an end-to-end
/// `minimum_receive`) or a direct caller (who must supply a price bound so
/// the swap is not sandwichable). Fail-closed: a factory/query error
/// propagates and rejects the swap rather than silently exempting.
pub fn query_registered_router(deps: Deps, factory_addr: &Addr) -> StdResult<Option<Addr>> {
    let resp: RegisteredRouterResponse = deps.querier.query_wasm_smart(
        factory_addr.to_string(),
        &FactoryQueryEnvelope::PoolFactoryQuery(FactoryQueryMsg::RegisteredRouter {}),
    )?;
    Ok(resp.router)
}

