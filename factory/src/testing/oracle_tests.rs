//! Pyth oracle tests.
//!
//! Exercises `usd_price::probe_pyth_usd_rate` end to end through the mock
//! Pyth querier: the happy-path read + normalization, and every fail-closed
//! gate (staleness, min-age, future-skew, wide confidence, out-of-range
//! exponent, non-positive price, plausibility band, unreachable contract).
//! The Pyth price is driven by `WasmMockQuerier::set_pyth_*`.

use cosmwasm_std::testing::{mock_env, MockApi};
use cosmwasm_std::{Coin, Decimal, Env, Uint128};

use crate::mock_querier::{mock_dependencies, WasmMockQuerier};
use crate::state::{FactoryInstantiate, FACTORYINSTANTIATEINFO};
use crate::usd_price::{probe_native_usd_rate, query_native_usd_rate};

const OSMO_USD_FEED: &str = "5867f5683c757393a0670ef0f701490950fe93fdb006d181c8265a831ac0c5c6";

fn make_addr(label: &str) -> cosmwasm_std::Addr {
    MockApi::default().addr_make(label)
}

fn pyth_config() -> FactoryInstantiate {
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
        pool_creation_fee: Uint128::new(1_000_000),
        gamm_pool_creation_fee: Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
        pyth_contract_addr: "pyth_oracle".to_string(),
        pyth_native_usd_feed_id: OSMO_USD_FEED.to_string(),
        max_pyth_staleness_seconds: 300,
        pyth_conf_threshold_bps: 200,
    }
}

/// An env whose block time is `age` seconds after `publish_time`, so the
/// mock feed reads with exactly that age.
fn env_at_age(publish_time: u64, age: u64) -> Env {
    let mut env = mock_env();
    env.block.time = cosmwasm_std::Timestamp::from_seconds(publish_time + age);
    env
}

#[test]
fn reads_osmo_usd_price_and_normalizes() {
    let mut deps = mock_dependencies(&[]);
    // OSMO/USD $0.03037 at expo -8 → price 3_037_100 → rate 30_371.
    deps.querier.set_pyth_price(3_037_100, -8, 100);
    deps.querier.set_pyth_publish_time(1_000_000);
    let env = env_at_age(1_000_000, 30);
    let rate = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap();
    assert_eq!(rate, Uint128::new(30_371), "6-dec USD/OSMO rate");
}

#[test]
fn dollar_one_via_stored_config() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000); // $1.00 expo -6
    deps.querier.set_pyth_publish_time(2_000_000);
    FACTORYINSTANTIATEINFO
        .save(deps.as_mut().storage, &pyth_config())
        .unwrap();
    let env = env_at_age(2_000_000, 60);
    let rate = query_native_usd_rate(deps.as_ref(), &env).unwrap();
    assert_eq!(rate, Uint128::new(1_000_000));
}

#[test]
fn stale_price_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_000);
    // age 301s > max staleness 300s.
    let env = env_at_age(1_000_000, 301);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("stale"), "got: {err}");
}

#[test]
fn too_fresh_price_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_000);
    // age 5s < MIN_PYTH_AGE 10s — the anti same-block-MEV floor.
    let env = env_at_age(1_000_000, 5);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("too fresh"), "got: {err}");
}

#[test]
fn future_publish_time_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_100);
    // publish_time 100s ahead of block time — beyond the 5s skew tolerance.
    let mut env = mock_env();
    env.block.time = cosmwasm_std::Timestamp::from_seconds(1_000_000);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("future"), "got: {err}");
}

#[test]
fn wide_confidence_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    // price 1_000_000, conf 30_000 = 3% > 200 bps (2%) gate.
    deps.querier.set_pyth_price(1_000_000, -6, 30_000);
    deps.querier.set_pyth_publish_time(1_000_000);
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("confidence"), "got: {err}");

    // ...but conf just under the gate (1.9%) passes.
    deps.querier.set_pyth_price(1_000_000, -6, 19_000);
    let rate = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap();
    assert_eq!(rate, Uint128::new(1_000_000));
}

#[test]
fn out_of_range_expo_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_price(1_000_000, -2, 0); // expo -2 outside [-12,-4]
    deps.querier.set_pyth_publish_time(1_000_000);
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("expo"), "got: {err}");
}

#[test]
fn nonpositive_price_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_price(0, -6, 0);
    deps.querier.set_pyth_publish_time(1_000_000);
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("negative or zero"), "got: {err}");
}

#[test]
fn above_rate_max_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    // $10_001 per OSMO at expo -6 = price 10_001_000_000, far over the $100
    // OSMO plausibility ceiling (which also catches a feed pointing at a
    // higher-priced asset).
    deps.querier.set_pyth_price(10_001_000_000, -6, 0);
    deps.querier.set_pyth_publish_time(1_000_000);
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("plausibility ceiling"), "got: {err}");
}

#[test]
fn unreachable_pyth_fails_closed() {
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_error("pyth contract unreachable");
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(!err.to_string().is_empty(), "must fail closed");
}

// --- Boundary tests: the gates must be exact, not approximate. ---

#[test]
fn staleness_boundary_is_exact() {
    let cfg = pyth_config(); // max_pyth_staleness_seconds = 300
    // age == max (300) passes; age == max+1 (301) fails.
    for (age, ok) in [(300u64, true), (301u64, false)] {
        let mut deps = mock_dependencies(&[]);
        deps.querier.set_pyth_usd_micro(1_000_000);
        deps.querier.set_pyth_publish_time(1_000_000);
        let env = env_at_age(1_000_000, age);
        let res = probe_native_usd_rate(deps.as_ref(), &env, &cfg);
        assert_eq!(res.is_ok(), ok, "age {age}: expected ok={ok}, got {res:?}");
    }
}

#[test]
fn min_age_boundary_is_exact() {
    let cfg = pyth_config();
    // age == MIN_PYTH_AGE (10) passes; age == 9 fails.
    for (age, ok) in [(10u64, true), (9u64, false)] {
        let mut deps = mock_dependencies(&[]);
        deps.querier.set_pyth_usd_micro(1_000_000);
        deps.querier.set_pyth_publish_time(1_000_000);
        let env = env_at_age(1_000_000, age);
        let res = probe_native_usd_rate(deps.as_ref(), &env, &cfg);
        assert_eq!(res.is_ok(), ok, "age {age}: expected ok={ok}, got {res:?}");
    }
}

#[test]
fn future_skew_boundary_is_exact() {
    let cfg = pyth_config();
    // publish_time == now + 5 (tolerance) is accepted (age saturates to 0,
    // but that trips the min-age gate, so it fails as "too fresh" not
    // "future") — the distinct thing to pin is now + 6 fails as FUTURE.
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_006); // 6s ahead
    let mut env = mock_env();
    env.block.time = cosmwasm_std::Timestamp::from_seconds(1_000_000);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &cfg).unwrap_err();
    assert!(err.to_string().contains("future"), "6s ahead must be future: {err}");
}

#[test]
fn conf_gate_boundary_is_exact() {
    let cfg = pyth_config(); // 200 bps = 2%
    // price 1_000_000, threshold = 200/10000 * 1e6 = 20_000.
    // conf == threshold (20_000) passes; conf == threshold+1 fails.
    for (conf, ok) in [(20_000u64, true), (20_001u64, false)] {
        let mut deps = mock_dependencies(&[]);
        deps.querier.set_pyth_price(1_000_000, -6, conf);
        deps.querier.set_pyth_publish_time(1_000_000);
        let env = env_at_age(1_000_000, 30);
        let res = probe_native_usd_rate(deps.as_ref(), &env, &cfg);
        assert_eq!(res.is_ok(), ok, "conf {conf}: expected ok={ok}, got {res:?}");
    }
}

#[test]
fn feed_id_mismatch_fails_closed() {
    // A Pyth contract that returns a DIFFERENT feed id than requested must
    // be rejected (defense-in-depth against a mis-routing contract).
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_000);
    deps.querier
        .set_pyth_feed_id_override("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef0");
    let env = env_at_age(1_000_000, 30);
    let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
    assert!(err.to_string().contains("feed_id mismatch"), "got: {err}");
}

#[test]
fn feed_id_match_is_case_insensitive() {
    // Config carries an UPPERCASE feed id; Pyth returns lowercase. Must match
    // (not fail closed) — the case-insensitivity fix.
    let mut cfg = pyth_config();
    cfg.pyth_native_usd_feed_id = OSMO_USD_FEED.to_uppercase();
    let mut deps = mock_dependencies(&[]);
    deps.querier.set_pyth_usd_micro(1_000_000);
    deps.querier.set_pyth_publish_time(1_000_000);
    // Mock echoes the requested id verbatim (uppercase). Force it lowercase
    // to model the real Pyth contract's canonical casing.
    deps.querier.set_pyth_feed_id_override(OSMO_USD_FEED); // lowercase
    let env = env_at_age(1_000_000, 30);
    let rate = probe_native_usd_rate(deps.as_ref(), &env, &cfg).unwrap();
    assert_eq!(rate, Uint128::new(1_000_000));
}

#[test]
fn expo_extremes_via_mock_price_correctly() {
    // expo -4 (price 25_000 = $2.50) and expo -12 (price 2_500_000_000_000
    // = $2.50) must both normalize to rate 2_500_000 through the full probe.
    for (price, expo) in [(25_000i64, -4i32), (2_500_000_000_000i64, -12i32)] {
        let mut deps = mock_dependencies(&[]);
        deps.querier.set_pyth_price(price, expo, 0);
        deps.querier.set_pyth_publish_time(1_000_000);
        let env = env_at_age(1_000_000, 30);
        let rate = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap();
        assert_eq!(rate, Uint128::new(2_500_000), "expo {expo} price {price}");
    }
}

#[test]
fn expo_out_of_range_via_mock_fails_closed() {
    for bad in [-3i32, -13i32] {
        let mut deps = mock_dependencies(&[]);
        deps.querier.set_pyth_price(1_000_000, bad, 0);
        deps.querier.set_pyth_publish_time(1_000_000);
        let env = env_at_age(1_000_000, 30);
        let err = probe_native_usd_rate(deps.as_ref(), &env, &pyth_config()).unwrap_err();
        assert!(err.to_string().contains("expo"), "expo {bad}: {err}");
    }
}

// Silence unused-import warning if the harness trims helpers.
#[allow(dead_code)]
fn _touch(_q: &WasmMockQuerier) {}
