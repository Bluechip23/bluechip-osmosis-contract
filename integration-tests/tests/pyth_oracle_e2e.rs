//! Pyth oracle — REAL execution against the embedded Osmosis chain.
//!
//! Replaces the retired multi-pool median/x-twap oracle E2E. The factory
//! now values every commit from the Pyth OSMO/USD feed, read from a
//! deployed (mock) Pyth CW contract with the exact wire shape of the real
//! one. These tests drive the live read + normalization and every
//! fail-closed gate (staleness, future-skew, wide confidence) on-chain, and
//! prove a real commit is valued at the Pyth rate.
//!
//! The mock Pyth stamps `publish_time` at push time, so advancing the chain
//! ages the price exactly as a real lagging keeper would — letting the
//! staleness gate be exercised for real.

mod common;

use common::*;
use cosmwasm_std::{Coin, Uint128};
use creator_pool::msg::{CommitStatus, QueryMsg as PoolQueryMsg};
use osmosis_test_tube::{Account, Gamm, Module, OsmosisTestApp, Wasm};

/// Stand up a factory whose USD price comes from a freshly-pushed mock Pyth
/// OSMO/USD feed at `usd_micro`. Returns (app-less) handles the test reuses.
fn setup(usd_micro: i64) -> (OsmosisTestApp, String, String, osmosis_test_tube::SigningAccount) {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&coins_sorted(vec![
            Coin::new(1_000_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ]))
        .unwrap();

    // A pricing pool still exists — but only as the fee-swap route.
    let gamm = Gamm::new(&app);
    let pricing_pool_id = gamm
        .create_basic_pool(
            &coins_sorted(vec![
                Coin::new(1_000_000_000u128, UOSMO),
                Coin::new(1_000_000_000u128, UUSDC),
            ]),
            &admin,
        )
        .unwrap()
        .data
        .pool_id;

    let wasm = Wasm::new(&app);
    let (factory_code_id, pool_code_id) = store_factory_and_pool(&wasm, &admin);
    let pyth = instantiate_mock_pyth(&wasm, store_mock_pyth(&wasm, &admin), &admin);
    // Push the price and age it past MIN_PYTH_AGE so instantiate's live
    // probe reads a fresh, consumable price.
    refresh_pyth(&app, &wasm, &pyth, &admin, usd_micro);

    let factory = instantiate_factory(
        &wasm,
        factory_code_id,
        &factory_init(
            &admin.address(),
            pricing_pool_id,
            pool_code_id,
            Coin::new(GAMM_CREATE_FEE, UOSMO),
            &pyth,
        ),
        &admin,
    );
    (app, factory, pyth, admin)
}

#[test]
fn pyth_prices_convert_native_to_usd_live() {
    // $2.50 per OSMO — a NON-unit rate a constant-return bug can't fake.
    let (app, factory, _pyth, _admin) = setup(2_500_000);
    let wasm = Wasm::new(&app);
    let conv = convert_native_to_usd(&wasm, &factory, 1_000_000).unwrap();
    assert_eq!(conv.rate_used, Uint128::new(2_500_000), "rate = $2.50/OSMO");
    assert_eq!(conv.amount, Uint128::new(2_500_000), "1 OSMO valued at $2.50");
    // 1000 OSMO → $2,500.
    let conv2 = convert_native_to_usd(&wasm, &factory, 1_000_000_000).unwrap();
    assert_eq!(conv2.amount, Uint128::new(2_500_000_000));
}

#[test]
fn real_commit_valued_at_pyth_rate() {
    // $2.50/OSMO; a 1000-OSMO commit must be ledgered at $2,500.
    let (app, factory, _pyth, admin) = setup(2_500_000);
    let wasm = Wasm::new(&app);
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let (pool_addr, _denom, _pid) = create_creator_pool(&wasm, &factory, &admin, "PYTHVAL");
    wasm.execute(
        &pool_addr,
        &commit_msg(1_000_000_000, None),
        &[Coin::new(1_000_000_000u128, UOSMO)],
        &committer,
    )
    .unwrap();
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::InProgress { raised, .. } if raised == Uint128::new(2_500_000_000)),
        "commit valued at the Pyth rate ($2.50 × 1000 OSMO = $2,500), got {status:?}"
    );
}

#[test]
fn stale_pyth_fails_closed_live() {
    let (app, factory, _pyth, _admin) = setup(1_000_000);
    let wasm = Wasm::new(&app);
    // Fresh now.
    assert!(convert_native_to_usd(&wasm, &factory, 1_000_000).is_ok());
    // Advance the chain past the 600s staleness window (keeper lapses).
    app.increase_time(700);
    let err = convert_native_to_usd(&wasm, &factory, 1_000_000).unwrap_err();
    assert!(
        err.to_string().contains("stale"),
        "a lagging keeper must fail closed, got: {err}"
    );
}

#[test]
fn future_and_wide_conf_fail_closed_live() {
    let (app, factory, pyth, admin) = setup(1_000_000);
    let wasm = Wasm::new(&app);
    // The chain's current time.
    let now = 1_000_000_000u64; // placeholder; recompute from block below
    let _ = now;

    // --- Future publish_time: 100s ahead of block time → refused. ---
    // Read the current block time via a fresh push, then set a future one.
    // (increase_time gives us a known reference: push sets publish_time=now.)
    // Use a very large explicit future timestamp relative to any plausible
    // test block time.
    set_pyth_at(&wasm, &pyth, &admin, 1_000_000, -6, 0, 4_000_000_000);
    let err = convert_native_to_usd(&wasm, &factory, 1_000_000).unwrap_err();
    assert!(
        err.to_string().contains("future"),
        "a far-future publish_time must be refused, got: {err}"
    );

    // --- Wide confidence: conf 3% > 200 bps gate → refused. ---
    // Push fresh (publish_time=now) with a wide conf, age it, then read.
    wasm.execute(
        &pyth,
        &mock_pyth::ExecuteMsg::SetPrice {
            price_id: OSMO_USD_FEED.to_string(),
            price: 1_000_000,
            expo: -6,
            conf: 30_000, // 3% of price
        },
        &[],
        &admin,
    )
    .unwrap();
    app.increase_time(15);
    let err = convert_native_to_usd(&wasm, &factory, 1_000_000).unwrap_err();
    assert!(
        err.to_string().contains("confidence"),
        "a wide confidence interval must be refused, got: {err}"
    );

    // ...and a tight conf just under the gate reads fine.
    refresh_pyth(&app, &wasm, &pyth, &admin, 1_000_000);
    assert!(convert_native_to_usd(&wasm, &factory, 1_000_000).is_ok());
}
