//! Swap-math re-exports plus the commit-phase USD-valuation client.
//!
//! The pure AMM math (`compute_swap`, `compute_offer_amount`,
//! `assert_max_spread`, `update_price_accumulator`, `DEFAULT_SLIPPAGE`)
//! lives in `pool_core::swap` and is re-exported below so imports like
//! `use crate::swap_helper::compute_swap;` resolve here.
pub use pool_core::swap::*;

use crate::state::POOL_INFO;
use cosmwasm_std::{Deps, StdError, StdResult, Uint128};
use pool_factory_interfaces::{ConversionResponse, FactoryQueryEnvelope, FactoryQueryMsg};

/// Fixed-point scale of `ConversionResponse.rate_used`: micro-USD per
/// micro-native. Must match `factory::usd_price::RATE_PRECISION`.
/// Duplicated rather than imported — the pool intentionally has no
/// compile-time factory dependency; the two communicate only over wasm
/// message boundaries.
pub const RATE_PRECISION: u128 = 1_000_000;

/// Values `native_amount` in USD via the factory, which computes the
/// price from Osmosis's chain-native x/twap over the configured
/// native/USD-stable pool. Returns the full `ConversionResponse` so the
/// caller can reuse `rate_used` for the inverse conversion inside the
/// same commit — one query per commit, no mid-tx rate drift.
///
/// Fail-closed: any error (factory unreachable, TWAP query failure)
/// propagates and reverts the commit rather than mispricing it. There is
/// no staleness window to check — the TWAP is computed against current
/// chain state at query time.
pub fn get_usd_conversion(deps: Deps, native_amount: Uint128) -> StdResult<ConversionResponse> {
    let factory_addr = POOL_INFO.load(deps.storage)?.factory_addr;
    deps.querier.query_wasm_smart(
        factory_addr,
        &FactoryQueryEnvelope::PoolFactoryQuery(FactoryQueryMsg::ConvertNativeToUsd {
            amount: native_amount,
        }),
    )
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
