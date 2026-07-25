//! Threshold-crossing ATOMICITY under real VM revert — a property that
//! cannot be proven by unit tests (MockStorage does not roll back on Err;
//! only a real chain reverts a failed tx).
//!
//! Strategy: force a REAL module-level failure in the MIDDLE of the
//! crossing's message chain — after the fee bank-sends and the three
//! TokenFactory `MsgMint`s have already EXECUTED — and assert that
//! everything rolls back: ledger writes, `IS_THRESHOLD_HIT`, minted
//! supply, fee transfers, and the crosser's attached OSMO.
//!
//! The forcing lever is the cross-denom creation-fee swap: the chain's
//! live `x/poolmanager` pool-creation fee is set (as governance would) to
//! MORE USDC than the pricing pool even holds, so the crossing's
//! `MsgSwapExactAmountOut` leg must fail INSIDE the gamm module, after
//! the mints ran. (Message order at crossing: fees/mints/refund →
//! fee-swap → create-pool[reply_on_success] → remit → notify; see
//! `threshold_crossing.rs`.)
//!
//! A second test exercises the sub-case where a live fee denom that is
//! NEITHER the native denom NOR the quote denom bricks every crossing
//! attempt (funds stay, nothing is taken) until governance restores the
//! fee — then the same pool crosses cleanly, proving the brick is
//! parameter-recoverable and not permanent state damage.

mod common;

use common::*;
use cosmwasm_std::{Coin, Decimal, Uint128};
use creator_pool::msg::{
    CommitStatus, ExecuteMsg as PoolExecuteMsg, NativePoolIdResponse, QueryMsg as PoolQueryMsg,
};
use osmosis_test_tube::{Account, Bank, Gamm, Module, OsmosisTestApp, Wasm};
use pool_factory_interfaces::asset::{TokenInfo, TokenType};

/// Total supply query via the factory (reads x/bank supply of the denom).
fn creator_supply(wasm: &Wasm<OsmosisTestApp>, factory: &str, pool_id: u64) -> Uint128 {
    let info: factory::query::CreatorTokenInfoResponse = wasm
        .query(
            factory,
            &factory::query::QueryMsg::CreatorTokenInfo { pool_id },
        )
        .unwrap();
    info.total_supply
}

#[test]
fn mid_crossing_module_failure_reverts_mints_ledger_fees_and_funds() {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&coins_sorted(vec![
            Coin::new(1_000_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ]))
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();

    // Pricing pool holds only 1,000 USDC — deliberately less than the fee
    // below, so the exact-out fee swap CANNOT succeed.
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
    app.increase_time(400);

    // Live chain fee: $2,000 USDC — twice what the pricing pool holds.
    set_pool_creation_fee(&app, UUSDC, 2_000_000_000);

    let wasm = Wasm::new(&app);
    let (factory_code_id, pool_code_id) = store_factory_and_pool(&wasm, &admin);
    let pyth = instantiate_mock_pyth(&wasm, store_mock_pyth(&wasm, &admin), &admin);
    refresh_pyth(&app, &wasm, &pyth, &admin, 1_000_000);
    let factory_addr = instantiate_factory(
        &wasm,
        factory_code_id,
        &factory_init(
            &admin.address(),
            pricing_pool_id,
            pool_code_id,
            // Factory config mirrors the chain fee (denom == usd_quote, so
            // config validation accepts it and reserve sizing engages).
            Coin::new(2_000_000_000u128, UUSDC),
            &pyth,
        ),
        &admin,
    );

    let (pool_addr, _denom, reg_pool_id) =
        create_creator_pool(&wasm, &factory_addr, &creator, "ATOMIC");
    let bank = Bank::new(&app);

    // Snapshots AFTER create, BEFORE the doomed commit. `creator` and
    // `admin` (the bluechip wallet) sign nothing in the commit tx, so their
    // balances must be EXACTLY unchanged if the 5%/1% fee sends roll back.
    let creator_osmo_before = balance(&bank, &creator.address(), UOSMO);
    let admin_osmo_before = balance(&bank, &admin.address(), UOSMO);
    let committer_before = balance(&bank, &committer.address(), UOSMO);
    assert_eq!(
        creator_supply(&wasm, &factory_addr, reg_pool_id),
        Uint128::zero()
    );

    // The crossing commit: at $1/OSMO, 26,000 OSMO crosses the $25k
    // threshold, so the full crossing chain dispatches — and its fee-swap
    // leg fails inside x/gamm.
    let err = wasm
        .execute(
            &pool_addr,
            &commit_msg(26_000_000_000, None),
            &[Coin::new(26_000_000_000u128, UOSMO)],
            &committer,
        )
        .unwrap_err();
    println!("mid-crossing module failure surfaced as: {err}");

    // --- EVERYTHING must have rolled back. ---
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::InProgress { raised, .. } if raised.is_zero()),
        "ledger + IS_THRESHOLD_HIT rolled back (still pre-threshold, raised 0), got {status:?}"
    );
    assert_eq!(
        creator_supply(&wasm, &factory_addr, reg_pool_id),
        Uint128::zero(),
        "the three EXECUTED TokenFactory mints (700B) rolled back with the tx"
    );
    let native: NativePoolIdResponse = wasm
        .query(&pool_addr, &PoolQueryMsg::NativePoolId {})
        .unwrap();
    assert!(native.pool_id.is_none(), "no native pool id recorded");
    assert_eq!(
        balance(&bank, &pool_addr, UOSMO),
        Uint128::zero(),
        "no OSMO stranded in the pool contract"
    );
    assert_eq!(
        balance(&bank, &pool_addr, UUSDC),
        Uint128::zero(),
        "no USDC stranded in the pool contract"
    );
    assert_eq!(
        balance(&bank, &creator.address(), UOSMO),
        creator_osmo_before,
        "creator 5% fee bank-send rolled back exactly"
    );
    assert_eq!(
        balance(&bank, &admin.address(), UOSMO),
        admin_osmo_before,
        "bluechip 1% fee send rolled back exactly"
    );
    let committer_after = balance(&bank, &committer.address(), UOSMO);
    let lost = committer_before.checked_sub(committer_after).unwrap();
    assert!(
        lost < Uint128::new(100_000_000),
        "crosser got the 26,000 OSMO back (lost {lost} — gas only)"
    );

    // --- Nothing is wedged: once governance parameters make the crossing
    // executable (fee back to 1000 OSMO native), the SAME pool crosses
    // cleanly and the reply records the native pool id. ---
    set_pool_creation_fee(&app, UOSMO, GAMM_CREATE_FEE);
    app.increase_time(30);
    wasm.execute(
        &pool_addr,
        &commit_msg(26_000_000_000, None),
        &[Coin::new(26_000_000_000u128, UOSMO)],
        &committer,
    )
    .expect("crossing must succeed once the live fee is payable");

    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(matches!(status, CommitStatus::FullyCommitted {}));
    let native: NativePoolIdResponse = wasm
        .query(&pool_addr, &PoolQueryMsg::NativePoolId {})
        .unwrap();
    let native_id = native
        .pool_id
        .expect("POOL_ID stored from the real create-pool reply");
    assert!(
        gamm.query_pool(native_id).is_ok(),
        "the recorded pool id resolves to a real gamm pool"
    );
    assert_eq!(
        creator_supply(&wasm, &factory_addr, reg_pool_id),
        Uint128::new(700_000_000_000),
        "exactly the three up-front mints landed this time"
    );

    // The stored POOL_ID makes the pool swappable: a belief-priced swap
    // routes through the freshly seeded native pool.
    app.increase_time(30);
    let swap = PoolExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(100_000_000),
        },
        belief_price: Some(Decimal::one()),
        max_spread: Some(Decimal::percent(5)),
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    wasm.execute(
        &pool_addr,
        &swap,
        &[Coin::new(100_000_000u128, UOSMO)],
        &committer,
    )
    .expect("post-recovery swap through the stored pool id");
}

/// The sharpest sub-case, live: governance re-denominates the pool
/// -creation fee into a coin the crossing cannot acquire (neither native
/// nor the quote denom). Every crossing attempt must revert with an
/// actionable error and TAKE NOTHING; when the fee is restored the same
/// pool crosses — the brick is exactly as wide as the parameter, no wider.
#[test]
fn unroutable_live_fee_denom_bricks_crossing_until_restored() {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&coins_sorted(vec![
            Coin::new(1_000_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ]))
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();

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
    app.increase_time(400);

    let wasm = Wasm::new(&app);
    let (factory_code_id, pool_code_id) = store_factory_and_pool(&wasm, &admin);
    let pyth = instantiate_mock_pyth(&wasm, store_mock_pyth(&wasm, &admin), &admin);
    refresh_pyth(&app, &wasm, &pyth, &admin, 1_000_000);
    let factory_addr = instantiate_factory(
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
    let (pool_addr, _denom, _reg_pool_id) =
        create_creator_pool(&wasm, &factory_addr, &creator, "BRICK");

    // Governance moves the live fee to an unroutable third denom.
    set_pool_creation_fee(&app, "uweird", 20_000_000);

    let bank = Bank::new(&app);
    let committer_before = balance(&bank, &committer.address(), UOSMO);

    let err = wasm
        .execute(
            &pool_addr,
            &commit_msg(26_000_000_000, None),
            &[Coin::new(26_000_000_000u128, UOSMO)],
            &committer,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("neither the native denom"),
        "crossing must fail with the actionable unroutable-fee error, got: {err}"
    );

    // The brick persists while the parameter stands…
    app.increase_time(30);
    let err = wasm
        .execute(
            &pool_addr,
            &commit_msg(26_000_000_000, None),
            &[Coin::new(26_000_000_000u128, UOSMO)],
            &committer,
        )
        .unwrap_err();
    assert!(err.to_string().contains("neither the native denom"));

    // …but takes nothing: funds returned, ledger untouched.
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::InProgress { raised, .. } if raised.is_zero()),
        "bricked crossing leaves the pool pre-threshold with an empty ledger, got {status:?}"
    );
    let lost = committer_before
        .checked_sub(balance(&bank, &committer.address(), UOSMO))
        .unwrap();
    assert!(
        lost < Uint128::new(100_000_000),
        "both failed attempts returned the principal (lost {lost} — gas only)"
    );

    // Governance restores the fee → the SAME pool crosses cleanly.
    set_pool_creation_fee(&app, UOSMO, GAMM_CREATE_FEE);
    app.increase_time(30);
    wasm.execute(
        &pool_addr,
        &commit_msg(26_000_000_000, None),
        &[Coin::new(26_000_000_000u128, UOSMO)],
        &committer,
    )
    .expect("crossing succeeds once the fee denom is routable again");
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(matches!(status, CommitStatus::FullyCommitted {}));
    let native: NativePoolIdResponse = wasm
        .query(&pool_addr, &PoolQueryMsg::NativePoolId {})
        .unwrap();
    assert!(native.pool_id.is_some());
}
