//! Swap-math re-exports plus the commit-phase USD-valuation client.
//!
//! The pure AMM math (`compute_swap`, `compute_offer_amount`,
//! `assert_max_spread`, `update_price_accumulator`, `DEFAULT_SLIPPAGE`)
//! lives in `pool_core::swap` and is re-exported below so imports like
//! `use crate::swap_helper::compute_swap;` resolve here.
pub use pool_core::swap::*;

use cosmwasm_std::{Addr, Deps, StdError, StdResult, Uint128};
use pool_factory_interfaces::{
    CommitContextResponse, FactoryQueryEnvelope, FactoryQueryMsg, RegisteredRouterResponse,
};

/// Fixed-point scale of `CommitContextResponse.rate_used`: micro-USD per
/// micro-native. Must match `factory::usd_price::RATE_PRECISION`.
/// Duplicated rather than imported — the pool intentionally has no
/// compile-time factory dependency; the two communicate only over wasm
/// message boundaries.
pub const RATE_PRECISION: u128 = 1_000_000;

/// F-3 — pool-side sanity CEILING on the factory-supplied native→USD rate
/// ($10,000 per native token). Mirrors `factory::usd_price::RATE_MAX`.
///
/// The factory already gates its TWAP read against this ceiling, so under
/// normal operation this NEVER fires. It exists as a defense-in-depth
/// firewall at the trust boundary: the pool delegates its entire valuation
/// to the factory, and every threshold / distribution calculation rides on
/// `rate_used`. A factory bug, a mis-set pricing pool, or a wrong-decimals
/// quote denom that slipped past the factory would otherwise let an absurd
/// rate value a dust commit as thousands of dollars and cross the threshold.
/// Only the ceiling is enforced (not a floor): an inflated rate is the theft
/// vector (dust crosses cheaply / steals distribution share), whereas a
/// deflated rate only makes crossing HARDER, so an asymmetric bound is
/// correct. `rate == 0` is rejected separately at the call site.
pub const POOL_RATE_MAX: u128 = 10_000 * RATE_PRECISION;

/// Values `native_amount` in USD via the factory, which computes the
/// price from Osmosis's chain-native x/twap over the configured
/// native/USD-stable pool, and returns the factory's live
/// `bluechip_wallet_address` in the same response — the two pieces of
/// factory state every commit needs, fetched in a single cross-contract
/// round-trip. The caller reuses `rate_used` for the inverse conversion
/// inside the same commit, so there is no mid-tx rate drift, and routes
/// the protocol fee / threshold-cross reward to `bluechip_wallet`, so an
/// admin wallet rotation takes effect for every existing pool without a
/// separate query.
///
/// Fail-closed: any error (factory unreachable, TWAP query failure)
/// propagates and reverts the commit rather than mispricing it. There is
/// no staleness window to check — the TWAP is computed against current
/// chain state at query time.
pub fn get_commit_context(
    deps: Deps,
    factory_addr: &Addr,
    native_amount: Uint128,
) -> StdResult<CommitContextResponse> {
    deps.querier.query_wasm_smart(
        factory_addr.to_string(),
        &FactoryQueryEnvelope::PoolFactoryQuery(FactoryQueryMsg::CommitContext {
            amount: native_amount,
        }),
    )
}

/// F-1 — query the factory for its registered multi-hop router address.
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

/// USD -> native using an already-captured rate. Exact inverse of the
/// factory's `native_to_usd` math (`usd * RATE_PRECISION / rate`), so
/// thresholding is arithmetically consistent with the valuation captured
/// at commit entry.
pub fn usd_to_native_at_rate(usd_amount: Uint128, rate: Uint128) -> StdResult<Uint128> {
    if rate.is_zero() {
        return Err(StdError::generic_err(
            "Cannot convert USD to native: rate is zero",
        ));
    }
    usd_amount
        .checked_mul(Uint128::from(RATE_PRECISION))
        .map_err(|e| StdError::generic_err(format!("Overflow converting USD to native: {}", e)))?
        .checked_div(rate)
        .map_err(|e| {
            StdError::generic_err(format!("Division error converting USD to native: {}", e))
        })
}
