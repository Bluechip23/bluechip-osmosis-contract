//! Multi-pool median oracle tests.
//!
//! Exercises `usd_price::probe_median_usd_rate` and its helpers: per-source
//! validation + discrediting, decimal normalization across mixed-decimal
//! quote denoms, the deviation filter, quorum fail-closed, and byte-identical
//! legacy behavior when no extra sources are configured. The per-pool x/twap
//! prices are driven by `WasmMockQuerier::set_twap_price_for_pool`.

use cosmwasm_std::testing::{message_info, mock_env, MockApi};
use cosmwasm_std::{Decimal, Uint128};

use crate::execute::instantiate;
use crate::mock_querier::{mock_dependencies, WasmMockQuerier};
use crate::state::{FactoryInstantiate, MultiOracleConfig, PricingSource, UsdLeg};
use crate::usd_price::{
    median_rate, probe_median_usd_rate, twap_dec_to_rate_with_decimals, twap_pair_to_rate,
};

fn make_addr(label: &str) -> cosmwasm_std::Addr {
    MockApi::default().addr_make(label)
}

/// Config with a primary pool (id 1, uusdc) plus the supplied extra sources
/// and oracle thresholds.
fn config_with_sources(
    extra_sources: Vec<PricingSource>,
    min_valid_sources: u32,
    max_deviation_bps: u64,
) -> FactoryInstantiate {
    FactoryInstantiate {
        factory_admin_address: make_addr("admin"),
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        cw721_nft_contract_id: 58,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: make_addr("bluechip"),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 14,
        bluechip_denom: "uosmo".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
        oracle: MultiOracleConfig {
            extra_sources,
            min_valid_sources,
            max_deviation_bps,
        },
    }
}

fn src(pool_id: u64, quote_denom: &str, quote_decimals: u32) -> PricingSource {
    PricingSource {
        pool_id,
        quote_denom: quote_denom.to_string(),
        quote_decimals,
        usd_leg: None,
    }
}

/// A routed source: `pool_id` prices native/`quote_denom`, then `leg_pool`
/// prices `quote_denom`/`usd_denom`.
fn routed_src(
    pool_id: u64,
    quote_denom: &str,
    leg_pool: u64,
    usd_denom: &str,
    usd_decimals: u32,
) -> PricingSource {
    PricingSource {
        pool_id,
        quote_denom: quote_denom.to_string(),
        quote_decimals: 0, // unused for routed sources (cancels in the product)
        usd_leg: Some(UsdLeg {
            pool_id: leg_pool,
            usd_denom: usd_denom.to_string(),
            usd_decimals,
        }),
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

#[test]
fn median_rate_odd_and_even() {
    assert_eq!(
        median_rate(&[Uint128::new(980_000), Uint128::new(1_000_000), Uint128::new(1_020_000)]),
        Uint128::new(1_000_000),
        "odd count → the middle element"
    );
    assert_eq!(
        median_rate(&[Uint128::new(980_000), Uint128::new(1_000_000)]),
        Uint128::new(990_000),
        "even count → floor-average of the two middle elements"
    );
    // Not order-dependent.
    assert_eq!(
        median_rate(&[Uint128::new(1_020_000), Uint128::new(980_000), Uint128::new(1_000_000)]),
        Uint128::new(1_000_000)
    );
}

/// A 6-decimal and an 18-decimal quote denom that both price the native asset
/// at exactly $1 must normalize to the SAME rate (1_000_000). This is the
/// load-bearing property that lets pools with different-decimal stables be
/// medianed together.
#[test]
fn mixed_decimal_quotes_normalize_to_the_same_usd_rate() {
    // 6-decimal quote, $1: TWAP quote_raw/base_raw = 1.0.
    assert_eq!(
        twap_dec_to_rate_with_decimals("1.000000000000000000", 6).unwrap(),
        Uint128::new(1_000_000)
    );
    // 18-decimal quote, $1: 1 native (1e6 uosmo) == 1 stable (1e18 units), so
    // TWAP = 1e18/1e6 = 1e12.
    assert_eq!(
        twap_dec_to_rate_with_decimals("1000000000000", 18).unwrap(),
        Uint128::new(1_000_000)
    );
    // Sanity ceiling still applies after normalization: a 6-decimal quote
    // reading 1e12 (the classic wrong-decimals inflation) is refused.
    assert!(twap_dec_to_rate_with_decimals("1000000000000", 6).is_err());
}

// ---------------------------------------------------------------------------
// Median aggregation over multiple pools
// ---------------------------------------------------------------------------

#[test]
fn median_of_three_valid_pools() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // $1.00
    deps.querier.set_twap_price_for_pool(2, "1.02"); // $1.02
    deps.querier.set_twap_price_for_pool(3, "0.98"); // $0.98

    let config = config_with_sources(vec![src(2, "uusdt", 6), src(3, "uaxlusdc", 6)], 1, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    assert_eq!(
        rate,
        Uint128::new(1_000_000),
        "median of $0.98/$1.00/$1.02 is $1.00"
    );
}

/// A dead source (query errors) is DISCREDITED, not fatal — the median is
/// taken over the survivors.
#[test]
fn dead_pool_is_discredited_and_median_taken_over_survivors() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00");
    deps.querier
        .set_twap_error_for_pool(2, "pool too young for window");
    deps.querier.set_twap_price_for_pool(3, "0.98");

    let config = config_with_sources(vec![src(2, "uusdt", 6), src(3, "uaxlusdc", 6)], 1, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Survivors {$1.00, $0.98} → even median = $0.99.
    assert_eq!(rate, Uint128::new(990_000));
}

/// A per-source SANITY failure (rate above the ceiling) discredits that source
/// too, exactly like a query error.
#[test]
fn source_above_sanity_ceiling_is_discredited() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00");
    // 6-decimal quote reading 1e12 → wrong-decimals / spiked → over RATE_MAX.
    deps.querier.set_twap_price_for_pool(2, "1000000000000");
    deps.querier.set_twap_price_for_pool(3, "1.04");

    let config = config_with_sources(vec![src(2, "uusdt", 6), src(3, "uaxlusdc", 6)], 1, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Pool 2 discredited → survivors {$1.00, $1.04} → $1.02.
    assert_eq!(rate, Uint128::new(1_020_000));
}

/// Quorum: when fewer than `min_valid_sources` survive validation, the whole
/// valuation FAILS CLOSED rather than pricing off a thin surviving set.
#[test]
fn quorum_not_met_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00");
    deps.querier.set_twap_error_for_pool(2, "dead");
    deps.querier.set_twap_error_for_pool(3, "dead");

    // Require 2 of 3 valid; only the primary survives → error.
    let config = config_with_sources(vec![src(2, "uusdt", 6), src(3, "uaxlusdc", 6)], 2, 0);
    let err = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap_err();
    assert!(
        err.to_string().contains("insufficient valid pricing sources"),
        "expected a quorum failure, got: {err}"
    );
}

/// The deviation filter discredits a pool that passed the absolute sanity gate
/// but disagrees with the consensus (a partially-manipulated pool), so it
/// cannot drag the median.
#[test]
fn deviation_filter_discredits_a_manipulated_pool() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // honest
    deps.querier.set_twap_price_for_pool(2, "1.01"); // honest
    deps.querier.set_twap_price_for_pool(3, "1.02"); // honest
    deps.querier.set_twap_price_for_pool(4, "10.00"); // manipulated (below ceiling)

    // 5% deviation band.
    let config = config_with_sources(
        vec![src(2, "a", 6), src(3, "b", 6), src(4, "c", 6)],
        2,
        500,
    );
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Provisional median of {1.00,1.01,1.02,10.00} is 1.015; pool 4 (10.00)
    // is >5% away and dropped, leaving {1.00,1.01,1.02} → median $1.01.
    assert_eq!(
        rate,
        Uint128::new(1_010_000),
        "the $10 manipulated pool must be discredited, median stays ~$1.01"
    );
}

/// With no extra sources and default thresholds, the median oracle reduces to
/// the single primary pool — byte-identical to the legacy single-pool path.
#[test]
fn single_primary_source_matches_legacy_behavior() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "3.50"); // $3.50/native

    let config = config_with_sources(vec![], 0, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    assert_eq!(rate, Uint128::new(3_500_000));

    // If the sole source dies, the whole valuation fails closed (same posture
    // as the pre-oracle single-pool code).
    deps.querier.set_twap_error_for_pool(1, "dead");
    assert!(probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).is_err());
}

// ---------------------------------------------------------------------------
// Routed (2-leg) sources: native/quote × quote/USD
// ---------------------------------------------------------------------------

/// The composite math: native priced in a volatile asset, times that asset
/// priced in USD, yields native-in-USD with the intermediate decimals
/// cancelling. D1=2.0 (native/quote) × D2=0.5 (quote/usd, 6-dec) = $1.00.
#[test]
fn routed_pair_normalization() {
    assert_eq!(
        twap_pair_to_rate("2.0", "0.5", 6).unwrap(),
        Uint128::new(1_000_000)
    );
    // The intermediate's own decimals do NOT appear — only the final USD
    // stable's. A dead/zero leg is rejected.
    assert!(twap_pair_to_rate("2.0", "0", 6).is_err());
}

/// Discriminating: the composite must produce the RIGHT non-unit value, not
/// just pass on cases that happen to equal $1.00 (which a "return constant" bug
/// would also pass). Asserts several distinct products.
#[test]
fn routed_pair_produces_correct_non_unit_rates() {
    // 3.0 × 0.5 = 1.5 → $1.50
    assert_eq!(twap_pair_to_rate("3.0", "0.5", 6).unwrap(), Uint128::new(1_500_000));
    // 0.5 × 0.5 = 0.25 → $0.25
    assert_eq!(twap_pair_to_rate("0.5", "0.5", 6).unwrap(), Uint128::new(250_000));
    // 8.0 × 0.25 = 2.0 → $2.00
    assert_eq!(twap_pair_to_rate("8.0", "0.25", 6).unwrap(), Uint128::new(2_000_000));
    // Realistic OSMO-priced-in-BTC × BTC-in-USDC: 0.0005 × 1000 = 0.5 → $0.50
    // (the 8-decimal BTC intermediate cancels — it is not even an input here).
    assert_eq!(
        twap_pair_to_rate("0.0005", "1000", 6).unwrap(),
        Uint128::new(500_000)
    );
}

/// A routed source with an identity USD leg (`D2 == 1.0`) must equal the direct
/// single-pool rate for the same native/quote TWAP — an internal consistency
/// check between the two code paths.
#[test]
fn routed_with_identity_leg_matches_direct() {
    for d1 in ["2.5", "0.75", "1.0", "9999.0"] {
        assert_eq!(
            twap_pair_to_rate(d1, "1.0", 6).unwrap(),
            twap_dec_to_rate_with_decimals(d1, 6).unwrap(),
            "routed with identity leg must equal the direct rate for {d1}"
        );
    }
}

/// Integration: a routed source carrying a NON-$1 price flows through the
/// median correctly. Both sources price OSMO at $2.00 → median $2.00 (a
/// return-constant bug would yield $1.00 and fail).
#[test]
fn routed_integration_carries_non_unit_price() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "2.00"); // primary USDC/OSMO, $2.00
    deps.querier.set_twap_price_for_pool(2, "8.0"); // OSMO/BTC
    deps.querier.set_twap_price_for_pool(20, "0.25"); // BTC/USDC → 8.0×0.25 = $2.00

    let config = config_with_sources(vec![routed_src(2, "ubtc", 20, "uusdc", 6)], 2, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    assert_eq!(rate, Uint128::new(2_000_000), "both sources price OSMO at $2.00");
}

/// A routed source (e.g. OSMO/BTC → BTC/USDC) contributes a USD price. Pool 2
/// prices native/BTC at 2.0, leg pool 20 prices BTC/USDC at 0.5 → $1.00.
#[test]
fn routed_source_prices_native_in_usd_via_second_leg() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // primary USDC/OSMO, $1
    deps.querier.set_twap_price_for_pool(2, "2.0"); // OSMO/BTC leg 1
    deps.querier.set_twap_price_for_pool(20, "0.5"); // BTC/USDC leg 2

    let config = config_with_sources(vec![routed_src(2, "ubtc", 20, "uusdc", 6)], 1, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Sources: primary $1.00, routed $1.00 → median $1.00.
    assert_eq!(rate, Uint128::new(1_000_000));
}

/// If EITHER leg of a routed source is dead, the whole source is discredited.
#[test]
fn routed_source_with_a_dead_leg_is_discredited() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // primary $1
    deps.querier.set_twap_price_for_pool(2, "2.0"); // leg 1 ok
    deps.querier.set_twap_error_for_pool(20, "leg pool too young"); // leg 2 dead
    deps.querier.set_twap_price_for_pool(3, "1.04"); // another direct source

    let config = config_with_sources(
        vec![routed_src(2, "ubtc", 20, "uusdc", 6), src(3, "uusdt", 6)],
        1,
        0,
    );
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Routed source discredited → survivors {primary $1.00, $1.04} → $1.02.
    assert_eq!(rate, Uint128::new(1_020_000));
}

/// End-to-end shape of the owner's intended set: USDC/OSMO direct + BTC/OSMO,
/// ATOM/OSMO, AKT/OSMO each routed through a USD leg. All ~$1 → median $1.
#[test]
fn mixed_direct_and_routed_set_medians_to_usd() {
    let mut deps = mock_dependencies(&[]);
    // Primary: USDC/OSMO direct, $1.00.
    deps.querier.set_twap_price_for_pool(1, "1.00");
    // BTC/OSMO (pool 2) × BTC/USDC (pool 20): 2.0 × 0.5 = $1.00.
    deps.querier.set_twap_price_for_pool(2, "2.0");
    deps.querier.set_twap_price_for_pool(20, "0.5");
    // ATOM/OSMO (pool 3) × ATOM/USDC (pool 30): 0.25 × 4.0 = $1.00.
    deps.querier.set_twap_price_for_pool(3, "0.25");
    deps.querier.set_twap_price_for_pool(30, "4.0");
    // AKT/OSMO (pool 4) × AKT/USDC (pool 40): 5.0 × 0.2 = $1.00.
    deps.querier.set_twap_price_for_pool(4, "5.0");
    deps.querier.set_twap_price_for_pool(40, "0.2");

    let config = config_with_sources(
        vec![
            routed_src(2, "ubtc", 20, "uusdc", 6),
            routed_src(3, "uatom", 30, "uusdc", 6),
            routed_src(4, "uakt", 40, "uusdc", 6),
        ],
        3,   // require a 3-of-4 quorum
        500, // ±5% deviation band
    );
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    assert_eq!(rate, Uint128::new(1_000_000), "four ~$1 sources median to $1.00");
}

/// A MANIPULATED routed source (its composite lands far from consensus) is
/// discredited by the deviation filter just like a direct outlier — proving the
/// filter operates on the final composite value, not the raw legs.
#[test]
fn manipulated_routed_source_is_deviation_filtered() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // primary $1
    deps.querier.set_twap_price_for_pool(2, "1.00"); // direct $1
    // Routed source manipulated to $8.00 (4.0 × 2.0), well outside a 5% band.
    deps.querier.set_twap_price_for_pool(3, "4.0");
    deps.querier.set_twap_price_for_pool(30, "2.0");

    let config = config_with_sources(
        vec![src(2, "uusdt", 6), routed_src(3, "ubtc", 30, "uusdc", 6)],
        2,
        500,
    );
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    // Provisional median of {1.00, 1.00, 8.00} is 1.00; the $8 routed source is
    // dropped, survivors {1.00, 1.00} → $1.00.
    assert_eq!(rate, Uint128::new(1_000_000));
}

/// An absurd routed composite (huge legs) fails the sanity ceiling and is
/// discredited rather than mispricing or panicking.
#[test]
fn absurd_routed_composite_is_discredited() {
    // 1e9 × 1e9 = 1e18 → far above the $10k/native ceiling.
    assert!(twap_pair_to_rate("1000000000", "1000000000", 6).is_err());

    let mut deps = mock_dependencies(&[]);
    deps.querier.set_twap_price_for_pool(1, "1.00"); // primary healthy
    deps.querier.set_twap_price_for_pool(2, "1000000000");
    deps.querier.set_twap_price_for_pool(20, "1000000000");
    // Only the primary survives; require just 1 → still prices at $1.00.
    let config = config_with_sources(vec![routed_src(2, "ubtc", 20, "uusdc", 6)], 1, 0);
    let rate = probe_median_usd_rate(deps.as_ref(), &mock_env(), &config).unwrap();
    assert_eq!(rate, Uint128::new(1_000_000));
}

// ---------------------------------------------------------------------------
// Config validation (propose/instantiate time)
// ---------------------------------------------------------------------------

#[test]
fn instantiate_rejects_malformed_usd_leg() {
    let mut deps = mock_dependencies(&[]);
    // Routed source whose leg pool id is zero.
    let bad = PricingSource {
        pool_id: 2,
        quote_denom: "ubtc".to_string(),
        quote_decimals: 0,
        usd_leg: Some(UsdLeg {
            pool_id: 0,
            usd_denom: "uusdc".to_string(),
            usd_decimals: 6,
        }),
    };
    let config = config_with_sources(vec![bad], 1, 0);
    let err = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&make_addr("admin"), &[]),
        config,
    )
    .unwrap_err();
    assert!(err.to_string().contains("usd_leg.pool_id"), "got: {err}");
}

#[test]
fn instantiate_rejects_malformed_extra_source() {
    let mut deps = mock_dependencies(&[]);
    // pool_id 0 is invalid.
    let config = config_with_sources(vec![src(0, "uusdt", 6)], 1, 0);
    let err = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&make_addr("admin"), &[]),
        config,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("pool_id must be non-zero"),
        "got: {err}"
    );
}

#[test]
fn instantiate_rejects_quorum_exceeding_source_count() {
    let mut deps = mock_dependencies(&[]);
    // 1 primary + 1 extra = 2 sources, but min_valid_sources = 3.
    let config = config_with_sources(vec![src(2, "uusdt", 6)], 3, 0);
    let err = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&make_addr("admin"), &[]),
        config,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("exceeds the"),
        "got: {err}"
    );
}

/// A pool id may appear only once across the source set (primary + extras),
/// so one manipulated pool cannot buy multiple correlated votes in the median.
#[test]
fn instantiate_rejects_duplicate_pool_id() {
    // Extra source reuses the primary pool id (1).
    let mut deps = mock_dependencies(&[]);
    let config = config_with_sources(vec![src(1, "uusdt", 6)], 1, 0);
    let err = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&make_addr("admin"), &[]),
        config,
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicate"), "got: {err}");

    // Two extra sources sharing a pool id.
    let mut deps = mock_dependencies(&[]);
    let config = config_with_sources(vec![src(2, "uusdt", 6), src(2, "uaxlusdc", 6)], 1, 0);
    let err = instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&make_addr("admin"), &[]),
        config,
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicate"), "got: {err}");
}

// Silence the unused-import warning for the querier type alias when the file
// is compiled in isolation.
#[allow(dead_code)]
fn _assert_querier_type(_q: &WasmMockQuerier) {}
