//! Native→USD valuation backed by the **Pyth** price oracle.
//!
//! Every commit values attached native (uosmo) against the
//! USD-denominated threshold. The price comes from Pyth's OSMO/USD feed,
//! read from the configured Pyth CW contract, gated for staleness,
//! confidence, future-skew and a minimum age (anti same-block-MEV), then
//! normalized to the `RATE_PRECISION` micro-USD-per-micro-native scale
//! that the whole system already speaks (`rate_used`).
//!
//! Why Pyth and not an on-chain pool TWAP: the Osmosis OSMO/USD pool
//! substrate is thin (a few thousand dollars of depth per venue), which
//! makes a pool-TWAP oracle manipulable for ~$1–3k. Pyth aggregates many
//! CEX/DEX venues, so moving its price is orders of magnitude costlier.
//!
//! Fail-closed: any query error, a stale/too-fresh/future price, a wide
//! confidence interval, or a failed sanity gate surfaces as `Err`, so a
//! commit that cannot be safely valued reverts rather than being mispriced.
//!
//! The pricing pool (`pricing_pool_id` / `usd_quote_denom`) is NO LONGER a
//! price source — it survives only as the cross-denom fee-swap EXECUTION
//! route at threshold crossing (acquiring the USDC pool-creation fee).

use cosmwasm_std::{Deps, Env, StdError, StdResult, Uint128};
use pool_factory_interfaces::ConversionResponse;

use crate::pyth_types::{PriceFeedResponse, PythQueryMsg};
use crate::state::{
    effective_max_staleness, effective_pyth_conf_bps, FactoryInstantiate, FACTORYINSTANTIATEINFO,
};

/// Fixed-point scale for `ConversionResponse.rate_used`: micro-USD per
/// micro-native. `1_000_000` == $1.00 per native token (both sides carry
/// 6 decimals, so the per-base-unit and per-token rates coincide).
pub const RATE_PRECISION: u128 = 1_000_000;

/// Plausibility band on the parsed native→USD rate, sized to this chain's
/// native asset (OSMO). A rate outside `[RATE_MIN, RATE_MAX]` means the
/// feed/expo is misconfigured, the configured feed id points at the WRONG
/// asset, or the price is being spoofed — fail closed.
///
/// The ceiling is deliberately tight. OSMO's all-time high is ~$11, so a
/// $100 ceiling leaves ~9x headroom yet still rejects a feed id typo'd to
/// a higher-priced asset — ETH (~$3k), SOL (~$150), BNB (~$600), BTC —
/// each of which is otherwise a perfectly valid, fresh, tight-confidence
/// Pyth feed that the hex-format / staleness / confidence gates cannot
/// tell apart from OSMO/USD. Without a tight band, mis-pointing the feed at
/// a ~$3k asset would value 1 uosmo at ~$3k and let an attacker cross a
/// large USD threshold for cents. The floor rejects a feed pointing at a
/// near-zero-priced asset.
pub const RATE_MAX: u128 = 100 * RATE_PRECISION;
pub const RATE_MIN: u128 = RATE_PRECISION / 10_000; // $0.0001 per native token

/// Maximum acceptable age (seconds) of the Pyth price relative to the
/// chain's block time — the staleness gate. A price older than this fails
/// closed. Mirrors the original integration's
/// `MAX_PRICE_AGE_SECONDS_BEFORE_STALE`.
pub const DEFAULT_MAX_PYTH_STALENESS_SECONDS: u64 = 300;
pub const MAX_PYTH_STALENESS_MIN_SECONDS: u64 = 30;
pub const MAX_PYTH_STALENESS_MAX_SECONDS: u64 = 600;

/// Minimum age (seconds), by Pyth publish_time, that a price must have
/// before it can be consumed. `publish_time` is Pyth's OFF-CHAIN signing
/// timestamp, not the on-chain block at which `UpdatePriceFeeds` stored the
/// price, so this gate does NOT prove the update and the commit landed in
/// different blocks. What it does guarantee is that the consumed price was
/// signed at least this long ago: a commit can therefore only ever be
/// valued against a price aged in `[MIN_PYTH_AGE_SECONDS, staleness]`, never
/// the just-signed tip. That bounds the price an adversary can select to
/// the range Pyth published over that window (further narrowed by the
/// confidence gate), so it removes the sharpest same-instant "push a
/// favorable tip and immediately consume it" edge without pretending to be
/// full cross-block separation. Unforgeability of Pyth signatures is what
/// actually defeats injecting a fake favorable price.
pub const MIN_PYTH_AGE_SECONDS: u64 = 10;

/// Clock-skew tolerance for a publish_time slightly ahead of block time.
pub const PYTH_FUTURE_SKEW_TOLERANCE_SECONDS: u64 = 5;

/// Confidence-interval gate bounds (basis points of price). A Pyth price
/// whose `conf/price` exceeds the configured bps is rejected (the feed is
/// too dispersed to trust). Admin-set value is clamped to this range.
pub const PYTH_CONF_THRESHOLD_BPS_DEFAULT: u16 = 200; // 2%
pub const PYTH_CONF_THRESHOLD_BPS_MIN: u16 = 50; // 0.5%
pub const PYTH_CONF_THRESHOLD_BPS_MAX: u16 = 500; // 5%

/// Allowed Pyth exponent range for a USD price feed.
pub const PYTH_EXPO_MIN: i32 = -12;
pub const PYTH_EXPO_MAX: i32 = -4;

/// Query the Pyth OSMO/USD rate for the stored factory config.
pub fn query_native_usd_rate(deps: Deps, env: &Env) -> StdResult<Uint128> {
    let config = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    probe_native_usd_rate(deps, env, &config)
}

/// Read the Pyth rate against an explicit (possibly not-yet-stored)
/// config. Split out from [`query_native_usd_rate`] so config validation
/// can probe a *proposed* Pyth route live at instantiate/propose/apply
/// time instead of discovering a typo'd contract/feed only when every
/// commit starts reverting.
pub fn probe_native_usd_rate(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
) -> StdResult<Uint128> {
    probe_pyth_usd_rate(deps, env, config)
}

/// The full Pyth read + validation pipeline, returning the native→USD
/// rate in `RATE_PRECISION` (micro-USD per micro-native) fixed point.
///
/// Pipeline (each step fails closed):
/// 1. Smart-query the configured Pyth contract for the native/USD feed.
/// 2. Verify the returned feed id matches the requested one (defense in
///    depth against a mis-routing Pyth contract).
/// 3. Reject a negative or far-future `publish_time`.
/// 4. Reject a price older than `max_pyth_staleness_seconds` (stale).
/// 5. Reject a price younger than `MIN_PYTH_AGE_SECONDS` (anti same-block MEV).
/// 6. Reject a non-positive price.
/// 7. Reject a confidence interval wider than the configured bps gate.
/// 8. Reject an out-of-range exponent.
/// 9. Normalize `price × 10^expo` to the 6-decimal `RATE_PRECISION` scale.
/// 10. Apply the shared `[RATE_MIN, RATE_MAX]` plausibility band.
pub fn probe_pyth_usd_rate(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
) -> StdResult<Uint128> {
    let feed_id = config.pyth_native_usd_feed_id.as_str();

    // 0. Refuse an unconfigured pricing route. A fresh instantiate always
    // supplies both fields (validated non-empty), but a factory upgraded in
    // place from a pre-Pyth serialized config deserializes these as the
    // serde default (empty string); fail closed with a clear message rather
    // than emit a confusing "contract not found" from the empty-address
    // query below. The admin recovers by proposing a config update that
    // sets the real Pyth contract/feed.
    if config.pyth_contract_addr.trim().is_empty() || feed_id.trim().is_empty() {
        return Err(StdError::generic_err(
            "Pyth pricing is not configured (empty contract address or feed id); \
             set it via a factory config update",
        ));
    }

    // 1. Query the Pyth contract.
    let response: PriceFeedResponse = deps.querier.query_wasm_smart(
        config.pyth_contract_addr.as_str(),
        &PythQueryMsg::PriceFeed {
            id: feed_id.to_string(),
        },
    )?;

    // 2. Extract the price, verifying the feed id. Case-INSENSITIVE: Pyth
    // returns the id lowercase, and config validation permits any hex case,
    // so a mixed-case config must still match rather than fail closed on
    // every read.
    let price_data = if let Some(feed) = response.price_feed {
        if !feed.id.eq_ignore_ascii_case(feed_id) {
            return Err(StdError::generic_err(format!(
                "Pyth response feed_id mismatch: requested {}, got {}",
                feed_id, feed.id
            )));
        }
        feed.price
    } else if response.price.is_some() {
        // A bare `price` with no `price_feed` wrapper carries no feed id, so
        // the feed-id-match gate above cannot run. The canonical Pyth CW
        // contract always answers `PriceFeed` with the `price_feed` variant;
        // a response using only the bare field is non-canonical / mis-routing.
        // Fail closed rather than trust a price we cannot attribute to the
        // requested feed.
        return Err(StdError::generic_err(
            "Pyth response carried a bare price with no feed id to verify; refusing it",
        ));
    } else {
        return Err(StdError::generic_err(
            "invalid Pyth response: missing price data",
        ));
    };

    let current_time = env.block.time.seconds();

    // 3. Reject negative / far-future publish_time.
    if price_data.publish_time < 0 {
        return Err(StdError::generic_err("Pyth publish_time is negative"));
    }
    let publish_time_u64 = price_data.publish_time as u64;
    if publish_time_u64 > current_time.saturating_add(PYTH_FUTURE_SKEW_TOLERANCE_SECONDS) {
        return Err(StdError::generic_err(
            "Pyth publish_time is in the future beyond the allowed skew tolerance",
        ));
    }

    // 4-5. Staleness + minimum-age gates.
    let age_seconds = current_time.saturating_sub(publish_time_u64);
    // Clamp at read time (defense-in-depth), mirroring `effective_pyth_conf_bps`
    // — config validation already range-checks this, but a direct state
    // write / bad migration must not be able to widen the staleness window.
    let max_staleness = effective_max_staleness(config);
    if age_seconds > max_staleness {
        return Err(StdError::generic_err(format!(
            "Pyth price is stale: age {}s exceeds max {}s",
            age_seconds, max_staleness
        )));
    }
    if age_seconds < MIN_PYTH_AGE_SECONDS {
        return Err(StdError::generic_err(format!(
            "Pyth price too fresh: age {}s below minimum {}s (forces cross-block \
             separation between UpdatePriceFeeds and this consumption to prevent \
             same-block bundled-update MEV; retry next block)",
            age_seconds, MIN_PYTH_AGE_SECONDS
        )));
    }

    // 6. Positive price. Load-bearing for the conf gate below — do not reorder.
    let price_i64 = price_data.price.i64();
    if price_i64 <= 0 {
        return Err(StdError::generic_err("Pyth price is negative or zero"));
    }
    // `try_into` (not `as`) so a future reordering of the guard above
    // produces an explicit error rather than a silent wrap.
    let price_u64: u64 = price_i64
        .try_into()
        .map_err(|_| StdError::generic_err("Pyth price overflow"))?;

    // 7. Confidence-interval gate.
    let conf_bps = effective_pyth_conf_bps(config);
    let conf_threshold = price_u64
        .saturating_mul(conf_bps as u64)
        .saturating_div(10_000);
    let conf_u64 = price_data.conf.u64();
    if conf_u64 > conf_threshold {
        return Err(StdError::generic_err(format!(
            "Pyth confidence interval too wide: conf={} exceeds {} bps of price={}",
            conf_u64, conf_bps, price_i64
        )));
    }

    // 8. Exponent range.
    let expo = price_data.expo;
    if !(PYTH_EXPO_MIN..=PYTH_EXPO_MAX).contains(&expo) {
        return Err(StdError::generic_err(format!(
            "unexpected Pyth expo {}: expected between {} and {}",
            expo, PYTH_EXPO_MIN, PYTH_EXPO_MAX
        )));
    }

    // 9. Normalize `price × 10^expo` into 6-decimal RATE_PRECISION units.
    // The normalized 6-decimal USD-per-token value IS `rate_used`, because
    // both the native denom and the rate carry 6 decimals.
    let rate = normalize_pyth_price_to_rate(price_u64.into(), expo)?;

    // 10. Shared sanity gate.
    apply_rate_sanity(rate, "Pyth price")
}

/// Normalize a raw Pyth `price` (with exponent `expo`, so the real value
/// is `price × 10^expo`) into a `RATE_PRECISION` (6-decimal) rate.
/// A price at expo `-6` maps 1:1; expo `< -6` divides out the extra
/// decimals; expo `> -6` multiplies up.
///
/// TOTAL (never panics): `expo` is gated to `[PYTH_EXPO_MIN, PYTH_EXPO_MAX]`
/// here as well as by `probe_pyth_usd_rate`, so the `6 - |expo|` /
/// `|expo| - 6` exponents below can never underflow and the `10^n` powers
/// stay bounded. Direct callers (tests, future code) get a fail-closed
/// `Err` on an out-of-range `expo` rather than a wasm trap.
pub fn normalize_pyth_price_to_rate(price: u128, expo: i32) -> StdResult<Uint128> {
    if !(PYTH_EXPO_MIN..=PYTH_EXPO_MAX).contains(&expo) {
        return Err(StdError::generic_err(format!(
            "Pyth expo {} out of range [{}, {}] for normalization",
            expo, PYTH_EXPO_MIN, PYTH_EXPO_MAX
        )));
    }
    let rate = match expo.cmp(&-6) {
        std::cmp::Ordering::Equal => price,
        std::cmp::Ordering::Less => {
            // expo < -6: value has more than 6 decimals; divide.
            let divisor = 10u128.pow((expo.abs() - 6) as u32);
            price / divisor
        }
        std::cmp::Ordering::Greater => {
            // expo > -6 (i.e. -5, -4): fewer decimals; multiply. `expo` is
            // gated to [-12,-4] above, so `unsigned_abs()` ∈ [4,12] and the
            // Greater branch only runs for {-5,-4} ⇒ `6 - |expo|` ∈ {1,2}.
            let multiplier = 10u128.pow(6 - expo.unsigned_abs());
            price
                .checked_mul(multiplier)
                .ok_or_else(|| StdError::generic_err("overflow normalizing Pyth price"))?
        }
    };
    Ok(Uint128::from(rate))
}

/// Apply the shared `[RATE_MIN, RATE_MAX]` plausibility band to a
/// normalized rate. Returns `Err` (⇒ fail closed) on any violation — a
/// zero/dust rate, a rate below the floor, or one above the OSMO ceiling
/// (which also catches a feed id pointing at a higher-priced asset).
pub fn apply_rate_sanity(rate: Uint128, ctx: &str) -> StdResult<Uint128> {
    if rate < Uint128::new(RATE_MIN) {
        return Err(StdError::generic_err(format!(
            "{ctx}: normalized rate {} (6-dec micro-USD/native) is below the plausibility \
             floor of {} (~$0.0001 per native; price too small, or the feed points at a \
             near-zero-priced asset)",
            rate, RATE_MIN,
        )));
    }
    if rate > Uint128::new(RATE_MAX) {
        return Err(StdError::generic_err(format!(
            "{ctx}: normalized rate {} exceeds the ${} per native plausibility ceiling \
             (feed/expo misconfigured, or the feed points at a higher-priced asset)",
            rate,
            RATE_MAX / RATE_PRECISION
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
    fn normalizes_expo_minus_8() {
        // OSMO/USD $0.03037 published at expo -8: price=3_037_100.
        // real = 3_037_100 × 10^-8 = $0.030371 → 6-dec rate 30_371.
        let rate = normalize_pyth_price_to_rate(3_037_100, -8).unwrap();
        assert_eq!(rate, Uint128::new(30_371));
    }

    #[test]
    fn normalizes_expo_minus_6_identity() {
        // $1.00 at expo -6 = price 1_000_000 → rate 1_000_000.
        let rate = normalize_pyth_price_to_rate(1_000_000, -6).unwrap();
        assert_eq!(rate, Uint128::new(1_000_000));
        assert_eq!(
            native_to_usd(Uint128::new(25_000_000_000), rate).unwrap(),
            Uint128::new(25_000_000_000)
        );
    }

    #[test]
    fn normalizes_expo_minus_5_multiplies() {
        // $2.50 at expo -5 = price 250_000 → rate 2_500_000.
        let rate = normalize_pyth_price_to_rate(250_000, -5).unwrap();
        assert_eq!(rate, Uint128::new(2_500_000));
    }

    #[test]
    fn sanity_rejects_zero_floor_and_ceiling() {
        // Zero and anything below the floor fail.
        assert!(apply_rate_sanity(Uint128::zero(), "x").is_err());
        assert!(apply_rate_sanity(Uint128::new(RATE_MIN - 1), "x").is_err());
        // The floor and ceiling themselves are accepted (inclusive band).
        assert_eq!(
            apply_rate_sanity(Uint128::new(RATE_MIN), "x").unwrap(),
            Uint128::new(RATE_MIN)
        );
        assert_eq!(
            apply_rate_sanity(Uint128::new(RATE_MAX), "x").unwrap(),
            Uint128::new(RATE_MAX)
        );
        assert!(apply_rate_sanity(Uint128::new(RATE_MAX + 1), "x").is_err());
    }

    #[test]
    fn wrong_asset_feed_rejected_by_ceiling() {
        // A feed id typo'd to ETH/USD (~$3,000) is a valid, fresh Pyth feed
        // but normalizes to a rate ~30x above the $100 OSMO ceiling, so the
        // plausibility band rejects it rather than valuing 1 uosmo at $3k.
        let eth = normalize_pyth_price_to_rate(3_000_000_000, -6).unwrap(); // $3,000
        assert!(apply_rate_sanity(eth, "eth").is_err());
        // SOL (~$150) is also above the $100 ceiling and rejected.
        let sol = normalize_pyth_price_to_rate(150_000_000, -6).unwrap();
        assert!(apply_rate_sanity(sol, "sol").is_err());
        // A realistic OSMO price ($0.03) sits comfortably inside the band.
        let osmo = normalize_pyth_price_to_rate(30_000, -6).unwrap(); // $0.03
        assert!(apply_rate_sanity(osmo, "osmo").is_ok());
    }

    #[test]
    fn dust_price_fails_closed() {
        // expo -12, price 1 → 1 / 10^6 = 0 → dust → reject.
        let rate = normalize_pyth_price_to_rate(1, -12).unwrap();
        assert_eq!(rate, Uint128::zero());
        assert!(apply_rate_sanity(rate, "x").is_err());
    }

    #[test]
    fn normalize_across_full_expo_range_is_consistent() {
        // A $1.00 price expressed at every allowed expo must normalize to the
        // SAME 6-dec rate (1_000_000). price = 1 * 10^|expo|.
        for expo in PYTH_EXPO_MIN..=PYTH_EXPO_MAX {
            let raw = 10u128.pow(expo.unsigned_abs());
            let rate = normalize_pyth_price_to_rate(raw, expo).unwrap();
            assert_eq!(
                rate,
                Uint128::new(1_000_000),
                "expo {expo}: $1.00 must normalize to 1_000_000, got {rate}"
            );
        }
    }

    #[test]
    fn normalize_rejects_out_of_range_expo_without_panicking() {
        // The pub fn must be TOTAL — no wasm trap on a hostile/out-of-range
        // expo, only a fail-closed Err. These would previously underflow /
        // overflow the power-of-ten arithmetic.
        for bad in [-13i32, -3, 0, 7, i32::MIN, i32::MAX] {
            let r = normalize_pyth_price_to_rate(1_000_000, bad);
            assert!(r.is_err(), "expo {bad} must Err, not panic/succeed");
        }
    }

    #[test]
    fn conf_gate_saturation_is_benign_huge_price_hits_rate_max() {
        // A near-i64::MAX price would saturate the conf-threshold multiply.
        // That is safe because such a price normalizes ABOVE RATE_MAX and is
        // rejected by the sanity ceiling regardless of the conf outcome.
        let huge = (i64::MAX as u128) - 1;
        // At expo -6 it maps ~1:1 → astronomically above RATE_MAX.
        let rate = normalize_pyth_price_to_rate(huge, -6).unwrap();
        assert!(apply_rate_sanity(rate, "x").is_err(), "huge price must fail RATE_MAX");
    }

    #[test]
    fn realistic_osmo_price_normalizes() {
        // Live Pyth OSMO/USD shape: price 3_193_516 at expo -8 = $0.03193516
        // → 6-dec rate 31_935 ($0.031935/OSMO).
        let rate = normalize_pyth_price_to_rate(3_193_516, -8).unwrap();
        assert_eq!(rate, Uint128::new(31_935));
        // Value a $0.25 threshold worth of OSMO: 0.25 / 0.031935 ≈ 7.8 OSMO
        // = 7_800_000 base units (6 decimals). * rate / 1e6 ≈ $0.249 micro-USD.
        let usd = native_to_usd(Uint128::new(7_800_000), rate).unwrap();
        assert!(usd >= Uint128::new(249_000) && usd <= Uint128::new(250_000), "got {usd}");
    }
}
