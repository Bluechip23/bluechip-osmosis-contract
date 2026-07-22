//! Native→USD valuation backed by Osmosis's chain-native `x/twap` module.
//!
//! A single stateless chain query: the arithmetic TWAP of the
//! factory-configured `bluechip_denom` / `usd_quote_denom` pool over
//! the last `twap_window_seconds`. The pairing asset is the chain's
//! native token, whose price against a USD stablecoin is maintained by
//! the chain itself — no keeper, no push liveness, and manipulating
//! the valuation requires moving one of Osmosis's deepest pools for
//! the entire TWAP window.
//!
//! Fail-closed: any TWAP query error (mis-configured pool id, pool too
//! young for the window, module pruning) surfaces as an error to the
//! caller, so a commit that cannot be valued reverts rather than being
//! mispriced.

use cosmwasm_std::{Decimal, Deps, Env, StdError, StdResult, Uint128, Uint256};
use osmosis_std::types::osmosis::twap::v1beta1::TwapQuerier;
use pool_factory_interfaces::ConversionResponse;

use crate::state::{FactoryInstantiate, PricingSource, FACTORYINSTANTIATEINFO};

/// Fixed-point scale for `ConversionResponse.rate_used`: micro-USD per
/// micro-native. `1_000_000` == $1.00 per native token (both sides carry
/// 6 decimals, so the per-base-unit and per-token rates coincide).
///
/// The 6/6-decimal assumption is load-bearing: `RATE_MAX` below exists
/// precisely to catch a quote asset that violates it.
pub const RATE_PRECISION: u128 = 1_000_000;

/// Sanity ceiling on the parsed rate: $10,000 per native token. No
/// plausible host-chain native asset trades anywhere near this, so a
/// rate above it means either the quote denom does not carry 6 decimals
/// (an 18-decimal stable inflates the rate ~1e12×, letting a dust
/// commit cross the USD threshold) or the pricing pool is being spiked.
/// Fail closed on both.
pub const RATE_MAX: u128 = 10_000 * RATE_PRECISION;

/// Lower/upper bounds on the configurable TWAP window. Below 300s a
/// single block carries enough weight in the arithmetic mean that a
/// one-block spike moves the rate materially — the manipulation cost
/// collapses toward a spot read; above 3600s the price lags real
/// markets enough to misvalue commits in fast moves (and approaches
/// the x/twap pruning horizon).
pub const TWAP_WINDOW_MIN_SECONDS: u64 = 300;
pub const TWAP_WINDOW_MAX_SECONDS: u64 = 3_600;

/// Query the chain's arithmetic TWAP for the configured pricing pool and
/// return the native→USD rate in `RATE_PRECISION` fixed point.
pub fn query_native_usd_rate(deps: Deps, env: &Env) -> StdResult<Uint128> {
    let config = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    probe_native_usd_rate(deps, env, &config)
}

/// Run the TWAP query against an explicit (possibly not-yet-stored)
/// config. Split out from [`query_native_usd_rate`] so config
/// validation can probe a *proposed* pricing route live at
/// instantiate/propose/apply time instead of discovering a typo'd pool
/// id only when every commit starts reverting.
pub fn probe_native_usd_rate(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
) -> StdResult<Uint128> {
    probe_median_usd_rate(deps, env, config)
}

/// The full ordered pricing-source set: the primary
/// `(pricing_pool_id, usd_quote_denom)` pool (6-decimal quote by
/// convention — it is also the cross-denom fee-swap route) followed by every
/// configured `oracle.extra_sources` entry.
pub fn pricing_sources(config: &FactoryInstantiate) -> Vec<PricingSource> {
    let mut sources = Vec::with_capacity(1 + config.oracle.extra_sources.len());
    sources.push(PricingSource {
        pool_id: config.pricing_pool_id,
        quote_denom: config.usd_quote_denom.clone(),
        quote_decimals: 6,
        // The primary pool is always a DIRECT USD-stable quote (it is also the
        // cross-denom fee-swap route). A routed primary is not supported.
        usd_leg: None,
    });
    sources.extend(config.oracle.extra_sources.iter().cloned());
    sources
}

/// Multi-pool MEDIAN native→USD rate.
///
/// Reads the arithmetic TWAP of every configured pricing source over the
/// window, normalizes each to the `RATE_PRECISION` (micro-USD per
/// micro-native) convention, and returns the MEDIAN of the sources that pass
/// validation. Design mirrors the internal multi-pool oracle in the original
/// bluechip-contracts:
///
/// 1. **Validate each source independently.** A source is DISCREDITED (simply
///    dropped, never fatal) if its x/twap query errors (typo'd pool id, pool
///    missing a denom, pool younger than the window) OR its parsed rate fails
///    the zero / sub-dust / `RATE_MAX` sanity gates. A single dead or spiked
///    pool therefore cannot take the whole valuation down.
/// 2. **Deviation discredit (optional).** When `oracle.max_deviation_bps > 0`,
///    compute a provisional median of the survivors and drop any source more
///    than that many bps away from it, then recompute. This ejects a pool that
///    passed the absolute sanity gate but disagrees with the consensus (a
///    partially-manipulated pool).
/// 3. **Quorum.** If fewer than `max(1, oracle.min_valid_sources)` sources
///    survive, FAIL CLOSED — no commit is priced — exactly the posture the
///    single-pool path already has when its one query fails.
/// 4. **Median.** Returned as the rate for the whole tx window (the caller
///    threads the SAME `rate_used` through every conversion in a commit).
///
/// Empty `extra_sources` + default thresholds ⇒ a single primary source,
/// median-of-one — byte-identical to the pre-oracle single-pool behavior.
pub fn probe_median_usd_rate(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
) -> StdResult<Uint128> {
    let sources = pricing_sources(config);
    let total = sources.len();

    // 1. Per-source query + validation. Discredited sources are dropped; their
    // reasons are retained so a quorum failure can tell the operator WHY each
    // source was rejected (e.g. wrong-decimals quote denom, dead pool).
    let mut valid: Vec<Uint128> = Vec::with_capacity(total);
    let mut discredited: Vec<String> = Vec::new();
    for source in &sources {
        match probe_single_source(deps, env, config, source) {
            Ok(rate) => valid.push(rate),
            Err(e) => discredited.push(format!("pool {}: {}", source.pool_id, e)),
        }
    }

    // 2. Optional deviation discredit against the provisional median.
    let mut deviation_dropped = 0usize;
    if config.oracle.max_deviation_bps > 0 && !valid.is_empty() {
        let provisional = median_rate(&valid);
        let max_bps = Uint256::from(config.oracle.max_deviation_bps);
        let med = Uint256::from(provisional);
        let before = valid.len();
        valid.retain(|rate| {
            let r = Uint256::from(*rate);
            let diff = if r > med { r - med } else { med - r };
            // |r - med| * 10_000 <= med * max_bps
            diff * Uint256::from(10_000u64) <= med * max_bps
        });
        deviation_dropped = before - valid.len();
    }

    // 3. Quorum. Fail closed when too few sources survive.
    let min_valid = config.oracle.min_valid_sources.max(1) as usize;
    if valid.len() < min_valid {
        return Err(StdError::generic_err(format!(
            "insufficient valid pricing sources: {} of {} configured survived validation \
             ({} deviation-dropped), need at least {} — refusing to price the commit. \
             Discredited: [{}]",
            valid.len(),
            total,
            deviation_dropped,
            min_valid,
            discredited.join("; ")
        )));
    }

    // 4. Median of survivors.
    Ok(median_rate(&valid))
}

/// Query the arithmetic TWAP of `base`/`quote` on `pool_id` over the config
/// window, returning the raw `Dec` string. Split out so both legs of a routed
/// source share one query path.
fn query_arithmetic_twap(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
    pool_id: u64,
    base: &str,
    quote: &str,
) -> StdResult<String> {
    let start_time = env
        .block
        .time
        .minus_seconds(config.twap_window_seconds)
        .seconds() as i64;
    let resp = TwapQuerier::new(&deps.querier)
        .arithmetic_twap_to_now(
            pool_id,
            base.to_string(),
            quote.to_string(),
            Some(osmosis_std::shim::Timestamp {
                seconds: start_time,
                nanos: 0,
            }),
        )
        .map_err(|e| {
            StdError::generic_err(format!(
                "x/twap query failed for pool {} ({}/{}, window {}s): {}",
                pool_id, base, quote, config.twap_window_seconds, e
            ))
        })?;
    Ok(resp.arithmetic_twap)
}

/// Query + normalize one pricing source into a `RATE_PRECISION` rate.
/// Returns `Err` (⇒ the source is discredited) on any query error or failed
/// sanity gate on EITHER leg.
///
/// - **Direct** source (`usd_leg == None`): `quote_denom` is a USD stable, so
///   the single native/quote TWAP is the USD rate (legacy behavior).
/// - **Routed** source (`usd_leg == Some`): the native/quote TWAP is combined
///   with a second quote/USD TWAP, so an OSMO/BTC (or OSMO/ATOM, …) pool can
///   contribute a USD price. Both legs must query and pass sanity or the whole
///   source is discredited.
pub fn probe_single_source(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
    source: &PricingSource,
) -> StdResult<Uint128> {
    // Leg 1: native priced in the source's quote denom.
    let d1 = query_arithmetic_twap(
        deps,
        env,
        config,
        source.pool_id,
        &config.bluechip_denom,
        &source.quote_denom,
    )?;

    match &source.usd_leg {
        None => twap_dec_to_rate_with_decimals(&d1, source.quote_decimals),
        Some(leg) => {
            // Leg 2: the intermediate (source.quote_denom) priced in USD.
            let d2 = query_arithmetic_twap(
                deps,
                env,
                config,
                leg.pool_id,
                &source.quote_denom,
                &leg.usd_denom,
            )?;
            twap_pair_to_rate(&d1, &d2, leg.usd_decimals)
        }
    }
}

/// Apply the shared zero / sub-dust / `RATE_MAX` sanity gates to a normalized
/// rate. Returns `Err` (⇒ discredit the source) on any violation.
fn apply_rate_sanity(rate: Uint128, ctx: &str) -> StdResult<Uint128> {
    if rate.is_zero() {
        return Err(StdError::generic_err(format!(
            "{ctx}: price too small for {RATE_PRECISION}-precision rate"
        )));
    }
    if rate > Uint128::new(RATE_MAX) {
        return Err(StdError::generic_err(format!(
            "{ctx}: price exceeds the ${} per native sanity ceiling — wrong-decimals \
             quote denom or manipulated pricing pool",
            RATE_MAX / RATE_PRECISION
        )));
    }
    Ok(rate)
}

/// Combine a routed source's two legs into a `RATE_PRECISION` rate:
/// `native_in_usd = TWAP(native/quote) × TWAP(quote/usd)`.
///
/// With `D1 = quote_raw/native_raw`, `D2 = usd_raw/quote_raw` and the native
/// denom fixed at 6 decimals, `rate = D1 × D2 × 10^(12 - usd_decimals)`. The
/// intermediate token's decimals cancel in the product, so only the USD
/// stable's `usd_decimals` matters. Computed as
/// `d1_atomics × d2_atomics / 10^(24 + usd_decimals)` in `Uint256`,
/// fail-closed on overflow.
pub fn twap_pair_to_rate(d1: &str, d2: &str, usd_decimals: u32) -> StdResult<Uint128> {
    let dec1: Decimal = d1
        .parse()
        .map_err(|e| StdError::generic_err(format!("cannot parse leg-1 twap \"{}\": {}", d1, e)))?;
    let dec2: Decimal = d2
        .parse()
        .map_err(|e| StdError::generic_err(format!("cannot parse leg-2 twap \"{}\": {}", d2, e)))?;
    if dec1.is_zero() || dec2.is_zero() {
        return Err(StdError::generic_err(
            "routed twap price has a zero leg — a pricing pool has no meaningful liquidity",
        ));
    }
    if usd_decimals > 30 {
        return Err(StdError::generic_err(format!(
            "usd_decimals {} is implausibly large",
            usd_decimals
        )));
    }
    // rate = d1_atomics * d2_atomics / 10^(24 + usd_decimals).
    let num = Uint256::from(dec1.atomics())
        .checked_mul(Uint256::from(dec2.atomics()))
        .map_err(|_| StdError::generic_err("overflow combining routed twap legs"))?;
    let den = Uint256::from(10u64).pow(24 + usd_decimals);
    let rate = Uint128::try_from(num / den)
        .map_err(|_| StdError::generic_err("routed twap price too large after normalization"))?;
    apply_rate_sanity(rate, "routed twap price")
}

/// Median of a non-empty slice of rates. Sorts a copy and returns the middle
/// element (odd count) or the floor-average of the two middle elements (even
/// count). Deterministic — no float, no `Math.random`. Panics only on an
/// empty slice, which callers guard against.
pub fn median_rate(rates: &[Uint128]) -> Uint128 {
    let mut sorted = rates.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        // Floor average of the two middle values via Uint256 to avoid any
        // intermediate overflow.
        let lo = Uint256::from(sorted[n / 2 - 1]);
        let hi = Uint256::from(sorted[n / 2]);
        Uint128::try_from((lo + hi) / Uint256::from(2u64)).unwrap_or(sorted[n / 2 - 1])
    }
}

/// Parse the x/twap module's 18-decimal `Dec` string (quote per base) into a
/// `RATE_PRECISION` fixed-point rate, normalizing for the quote denom's
/// decimal count.
///
/// The x/twap price `D` is `quote_raw / base_raw`. With the native (base)
/// denom fixed at 6 decimals and the quote denom carrying `q` decimals, the
/// USD-per-native rate in `RATE_PRECISION` units is `D * 10^(12 - q)`:
/// - `q == 6` reduces to `D * RATE_PRECISION` — the original 6/6 behavior;
/// - `q == 18` (an 18-decimal bridged stable) divides out the extra 1e12 so
///   an honest $1 price still reads `1_000_000`.
pub fn twap_dec_to_rate_with_decimals(twap: &str, quote_decimals: u32) -> StdResult<Uint128> {
    let dec: Decimal = twap
        .parse()
        .map_err(|e| StdError::generic_err(format!("cannot parse twap dec \"{}\": {}", twap, e)))?;
    if dec.is_zero() {
        return Err(StdError::generic_err(
            "twap price is zero — pricing pool has no meaningful liquidity",
        ));
    }
    // Bound the exponent so an absurd `quote_decimals` cannot build a giant
    // power of ten. No real denom exceeds ~24 decimals.
    if quote_decimals > 30 {
        return Err(StdError::generic_err(format!(
            "quote_decimals {} is implausibly large",
            quote_decimals
        )));
    }

    // rate = D * 10^(12 - q). Work from `dec.atomics()` (= D * 1e18) in
    // Uint256 so neither a high-decimal quote nor a large price overflows.
    //   rate = atomics * 10^(12 - q) / 1e18
    // Split into a multiply and a divide that are each always non-negative.
    let atomics = Uint256::from(dec.atomics()); // D * 1e18
    let ten = Uint256::from(10u64);
    let pow = |n: u32| -> Uint256 { ten.pow(n) };

    // numerator exponent and denominator exponent of 10, kept >= 0.
    // rate = atomics * 10^num / 10^den where num - den = (12 - q) - 18 = -(6 + q)? -- derive directly:
    // rate = atomics * 10^(12 - q) / 10^18
    //  q <= 12:  num = 12 - q, den = 18
    //  q  > 12:  num = 0,      den = 18 + (q - 12) = 6 + q
    let (num_exp, den_exp) = if quote_decimals <= 12 {
        (12 - quote_decimals, 18u32)
    } else {
        (0u32, 6 + quote_decimals)
    };
    let scaled = atomics
        .checked_mul(pow(num_exp))
        .map_err(|_| StdError::generic_err("overflow normalizing twap price"))?;
    let rate256 = scaled / pow(den_exp);
    let rate = Uint128::try_from(rate256)
        .map_err(|_| StdError::generic_err("twap price too large after normalization"))?;

    apply_rate_sanity(rate, &format!("twap price {} (quote decimals {})", twap, quote_decimals))
}

/// Parse the x/twap `Dec` string assuming a 6-decimal quote denom (the
/// primary-pool convention). Thin wrapper over
/// [`twap_dec_to_rate_with_decimals`].
pub fn twap_dec_to_rate(twap: &str) -> StdResult<Uint128> {
    twap_dec_to_rate_with_decimals(twap, 6)
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
    fn rejects_rates_above_sanity_ceiling() {
        // Exactly at the ceiling is accepted...
        assert_eq!(twap_dec_to_rate("10000").unwrap(), Uint128::new(RATE_MAX));
        // ...one micro-USD above is refused.
        let err = twap_dec_to_rate("10000.000001").unwrap_err();
        assert!(err.to_string().contains("sanity ceiling"), "{}", err);
        // The wrong-decimals scenario: an 18-decimal quote denom
        // inflates a $1 price to ~1e12 — must be refused, not used to
        // value commits.
        assert!(twap_dec_to_rate("1000000000000").is_err());
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
