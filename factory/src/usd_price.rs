//! Native→USD valuation backed by Osmosis's chain-native `x/twap` module.
//!
//! This replaces the old multi-thousand-line internal oracle (anchor pool
//! TWAP × Pyth, circuit breakers, keeper updates) with a single stateless
//! chain query: the arithmetic TWAP of the factory-configured
//! `bluechip_denom` / `usd_quote_denom` pool over the last
//! `twap_window_seconds`. The old oracle existed because the protocol's
//! own token had no external price; on Osmosis the pairing asset is the
//! chain's native token, whose price against a USD stablecoin is
//! maintained by the chain itself — no keeper, no push liveness, and
//! manipulating it requires moving one of Osmosis's deepest pools for the
//! entire TWAP window.
//!
//! Fail-closed: any TWAP query error (mis-configured pool id, pool too
//! young for the window, module pruning) surfaces as an error to the
//! caller, so a commit that cannot be valued reverts rather than being
//! mispriced.

use cosmwasm_std::{Decimal, Deps, Env, StdError, StdResult, Uint128};
use osmosis_std::types::osmosis::twap::v1beta1::TwapQuerier;
use pool_factory_interfaces::ConversionResponse;

use crate::state::FACTORYINSTANTIATEINFO;

/// Fixed-point scale for `ConversionResponse.rate_used`: micro-USD per
/// micro-native. `1_000_000` == $1.00 per native token (both sides carry
/// 6 decimals, so the per-base-unit and per-token rates coincide).
pub const RATE_PRECISION: u128 = 1_000_000;

/// Lower/upper bounds on the configurable TWAP window. Below 60s the
/// manipulation cost collapses toward a single-block spot read; above
/// 3600s the price lags real markets enough to misvalue commits in
/// fast moves (and approaches the x/twap pruning horizon).
pub const TWAP_WINDOW_MIN_SECONDS: u64 = 60;
pub const TWAP_WINDOW_MAX_SECONDS: u64 = 3_600;

/// Query the chain's arithmetic TWAP for the configured pricing pool and
/// return the native→USD rate in `RATE_PRECISION` fixed point.
pub fn query_native_usd_rate(deps: Deps, env: &Env) -> StdResult<Uint128> {
    let config = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    let start_time = env
        .block
        .time
        .minus_seconds(config.twap_window_seconds)
        .seconds() as i64;

    let resp = TwapQuerier::new(&deps.querier)
        .arithmetic_twap_to_now(
            config.pricing_pool_id,
            config.bluechip_denom.clone(),
            config.usd_quote_denom.clone(),
            Some(osmosis_std::shim::Timestamp {
                seconds: start_time,
                nanos: 0,
            }),
        )
        .map_err(|e| {
            StdError::generic_err(format!(
                "x/twap query failed for pool {} ({}/{}, window {}s): {}",
                config.pricing_pool_id,
                config.bluechip_denom,
                config.usd_quote_denom,
                config.twap_window_seconds,
                e
            ))
        })?;

    twap_dec_to_rate(&resp.arithmetic_twap)
}

/// Parse the x/twap module's 18-decimal `Dec` string (quote per base,
/// i.e. micro-USD per micro-native when both denoms carry 6 decimals)
/// into a `RATE_PRECISION` fixed-point rate.
pub fn twap_dec_to_rate(twap: &str) -> StdResult<Uint128> {
    let dec: Decimal = twap
        .parse()
        .map_err(|e| StdError::generic_err(format!("cannot parse twap dec \"{}\": {}", twap, e)))?;
    if dec.is_zero() {
        return Err(StdError::generic_err(
            "twap price is zero — pricing pool has no meaningful liquidity",
        ));
    }
    let rate = Uint128::new(RATE_PRECISION).mul_floor(dec);
    if rate.is_zero() {
        // Sub-1e-6 price: representable by Dec but truncates to a zero
        // rate. Refuse rather than valuing every commit at $0.
        return Err(StdError::generic_err(format!(
            "twap price {} too small for {}-precision rate",
            twap, RATE_PRECISION
        )));
    }
    Ok(rate)
}

/// Value `native_amount` (base units) in micro-USD at `rate`.
pub fn native_to_usd(native_amount: Uint128, rate: Uint128) -> StdResult<Uint128> {
    native_amount
        .checked_mul(rate)
        .map_err(|e| StdError::generic_err(format!("overflow valuing commit in USD: {}", e)))?
        .checked_div(Uint128::new(RATE_PRECISION))
        .map_err(|e| StdError::generic_err(format!("division error valuing commit: {}", e)))
}

/// Full conversion for the `ConvertNativeToUsd` factory query.
pub fn convert_native_to_usd(
    deps: Deps,
    env: &Env,
    amount: Uint128,
) -> StdResult<ConversionResponse> {
    let rate = query_native_usd_rate(deps, env)?;
    Ok(ConversionResponse {
        amount: native_to_usd(amount, rate)?,
        rate_used: rate,
        timestamp: env.block.time.seconds(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_dollar_rate() {
        // x/twap Dec "1.000000000000000000" == $1 per token == rate 1e6.
        let rate = twap_dec_to_rate("1.000000000000000000").unwrap();
        assert_eq!(rate, Uint128::new(1_000_000));
        assert_eq!(
            native_to_usd(Uint128::new(25_000_000_000), rate).unwrap(),
            Uint128::new(25_000_000_000)
        );
    }

    #[test]
    fn parses_fractional_rate() {
        // $0.50 per token: 2 native == 1 USD.
        let rate = twap_dec_to_rate("0.5").unwrap();
        assert_eq!(rate, Uint128::new(500_000));
        assert_eq!(
            native_to_usd(Uint128::new(2_000_000), rate).unwrap(),
            Uint128::new(1_000_000)
        );
    }

    #[test]
    fn rejects_zero_and_dust_rates() {
        assert!(twap_dec_to_rate("0").is_err());
        // 1e-7 truncates below the 1e6 fixed point.
        assert!(twap_dec_to_rate("0.0000001").is_err());
        assert!(twap_dec_to_rate("not-a-number").is_err());
    }

    #[test]
    fn round_trips_with_inverse_at_same_rate() {
        // native -> usd -> native at the same rate loses at most 1 base
        // unit to truncation (the pool-side inverse is
        // usd * RATE_PRECISION / rate).
        let rate = twap_dec_to_rate("3.141592000000000000").unwrap();
        let native = Uint128::new(123_456_789);
        let usd = native_to_usd(native, rate).unwrap();
        let back = usd
            .checked_mul(Uint128::new(RATE_PRECISION))
            .unwrap()
            .checked_div(rate)
            .unwrap();
        assert!(native.checked_sub(back).unwrap() <= Uint128::new(1));
    }
}
