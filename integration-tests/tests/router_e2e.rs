//! Router + F-1 belief-price gate, END TO END on a real chain:
//!
//!   1. a direct null-belief `SimpleSwap` is refused while no router is
//!      registered (fail-closed),
//!   2. a belief-priced direct swap works, a TIGHT belief price makes the
//!      REAL poolmanager swap revert on its `token_out_min_amount` floor
//!      (whole tx, funds returned),
//!   3. a post-threshold `Commit` without belief_price is refused (H-3),
//!   4. `ProposeRouter` is admin-only; a PENDING router has no effect on
//!      pools; `ApplyRouter` before the 48h window is refused; after
//!      `increase_time(48h)` it lands and `RegisteredRouter` reflects it,
//!   5. the registered router's two-hop route (creatorA → OSMO → creatorB,
//!      null belief per hop, real `MsgSwapExactAmountIn` on two seeded
//!      native pools) delivers ≥ `minimum_receive` to the caller,
//!   6. an impossible `minimum_receive` reverts the WHOLE route (real
//!      multi-message atomicity — hop swaps already executed roll back and
//!      the input comes back), and
//!   7. the very next route succeeds — the F-5 `ROUTE_IN_PROGRESS` guard
//!      does not wedge after a real on-chain revert.
//!
//! This is run on the PRODUCTION factory bytes (real 172,800s timelock);
//! the chain clock is advanced with `increase_time`, so the test proves
//! the prod constant, not a shortened test build.

mod common;

use common::*;
use cosmwasm_std::{Coin, Decimal, Uint128};
use creator_pool::msg::{CommitStatus, ExecuteMsg as PoolExecuteMsg, QueryMsg as PoolQueryMsg};
use factory::msg::ExecuteMsg as FactoryExecuteMsg;
use osmosis_test_tube::{Account, Bank, Gamm, Module, OsmosisTestApp, Wasm};
use pool_factory_interfaces::asset::{TokenInfo, TokenType};
use pool_factory_interfaces::routing::SwapOperation;

/// Prod factory admin timelock (`ADMIN_TIMELOCK_SECONDS`): 48 hours.
const TIMELOCK_SECONDS: u64 = 172_800;

fn simple_swap_msg(amount: u128, belief_price: Option<Decimal>) -> PoolExecuteMsg {
    PoolExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(amount),
        },
        belief_price,
        max_spread: Some(Decimal::percent(5)),
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    }
}

#[test]
fn router_timelock_belief_gate_and_no_wedge_end_to_end() {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&coins_sorted(vec![
            Coin::new(1_000_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ]))
        .unwrap();
    let creator_a = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let creator_b = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let crosser_a = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let crosser_b = app
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
    let router_code_id = wasm
        .store_code(&read_wasm("router.wasm"), None, &admin)
        .unwrap()
        .data
        .code_id;

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
    let router_addr = wasm
        .instantiate(
            router_code_id,
            &router::msg::InstantiateMsg {
                factory_addr: factory_addr.clone(),
                bluechip_denom: UOSMO.to_string(),
                admin: admin.address(),
            },
            Some(&admin.address()),
            Some("router"),
            &[],
            &admin,
        )
        .unwrap()
        .data
        .address;

    // Two crossed pools (distinct creators sidestep the 1-pool/hr
    // per-sender prod rate limit; distinct crossers sidestep commit rate
    // limits).
    let (pool_a, denom_a, _pid_a) = create_creator_pool(&wasm, &factory_addr, &creator_a, "HOPA");
    let (pool_b, denom_b, _pid_b) = create_creator_pool(&wasm, &factory_addr, &creator_b, "HOPB");
    for (pool, crosser) in [(&pool_a, &crosser_a), (&pool_b, &crosser_b)] {
        wasm.execute(
            pool,
            &commit_msg(26_000_000_000, None),
            &[Coin::new(26_000_000_000u128, UOSMO)],
            crosser,
        )
        .unwrap();
        let status: CommitStatus = wasm.query(pool, &PoolQueryMsg::IsFullyCommited {}).unwrap();
        assert!(matches!(status, CommitStatus::FullyCommitted {}));
        drain_distribution(&app, &wasm, pool, crosser);
    }
    let bank = Bank::new(&app);
    let crosser_a_tokens = balance(&bank, &crosser_a.address(), &denom_a);
    assert!(
        !crosser_a_tokens.is_zero(),
        "crosser A holds creator-A tokens from distribution (route input)"
    );
    app.increase_time(30);

    // --- (1) F-1 fail-closed: no router registered ⇒ a direct null-belief
    // SimpleSwap is refused by the live pool. ---
    let err = wasm
        .execute(
            &pool_a,
            &simple_swap_msg(100_000_000, None),
            &[Coin::new(100_000_000u128, UOSMO)],
            &crosser_a,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("belief_price is required"),
        "null-belief direct swap must be refused with no router registered, got: {err}"
    );

    // --- (2a) A belief-priced direct swap works (control). ---
    let before = balance(&bank, &crosser_a.address(), &denom_a);
    wasm.execute(
        &pool_a,
        &simple_swap_msg(100_000_000, Some(Decimal::one())),
        &[Coin::new(100_000_000u128, UOSMO)],
        &crosser_a,
    )
    .unwrap();
    assert!(
        balance(&bank, &crosser_a.address(), &denom_a) > before,
        "belief-priced direct swap delivered creator tokens"
    );
    app.increase_time(30);

    // --- (2b) A TIGHT belief price must make the REAL swap revert via the
    // dispatched `token_out_min_amount` floor: belief $0.000001/token ⇒
    // floor ~9.5e13 tokens for a 100-OSMO offer — unpayable. Funds return.
    let osmo_before = balance(&bank, &crosser_a.address(), UOSMO);
    let tok_before = balance(&bank, &crosser_a.address(), &denom_a);
    let err = wasm
        .execute(
            &pool_a,
            &simple_swap_msg(100_000_000, Some(Decimal::from_ratio(1u128, 1_000_000u128))),
            &[Coin::new(100_000_000u128, UOSMO)],
            &crosser_a,
        )
        .unwrap_err();
    println!("tight-belief swap refused by the chain as: {err}");
    assert_eq!(
        balance(&bank, &crosser_a.address(), &denom_a),
        tok_before,
        "no tokens delivered on the reverted tight swap"
    );
    let lost = osmo_before
        .checked_sub(balance(&bank, &crosser_a.address(), UOSMO))
        .unwrap();
    assert!(
        lost < Uint128::new(100_000_000),
        "the 100-OSMO offer came back (lost {lost} — gas only)"
    );
    app.increase_time(30);

    // --- (3) Post-threshold Commit requires belief_price (H-3), live. ---
    let err = wasm
        .execute(
            &pool_a,
            &commit_msg(1_000_000_000, None),
            &[Coin::new(1_000_000_000u128, UOSMO)],
            &crosser_a,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("belief_price is required"),
        "post-threshold commit without belief_price must be refused, got: {err}"
    );

    // --- (4) Router registration is admin-only + 48h-timelocked. ---
    let err = wasm
        .execute(
            &factory_addr,
            &FactoryExecuteMsg::ProposeRouter {
                router: router_addr.clone(),
            },
            &[],
            &creator_a,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("Unauthorized"),
        "non-admin ProposeRouter must be refused, got: {err}"
    );

    wasm.execute(
        &factory_addr,
        &FactoryExecuteMsg::ProposeRouter {
            router: router_addr.clone(),
        },
        &[],
        &admin,
    )
    .unwrap();

    // Pending ⇒ not registered: the query reflects only APPLIED state…
    let reg: pool_factory_interfaces::RegisteredRouterResponse = wasm
        .query(
            &factory_addr,
            &factory::query::QueryMsg::PoolFactoryQuery(
                pool_factory_interfaces::FactoryQueryMsg::RegisteredRouter {},
            ),
        )
        .unwrap();
    assert!(reg.router.is_none(), "pending proposal must not register");

    // …and a route attempted NOW dies on the pools' belief gate (the
    // router swaps null-belief and is not yet exempt).
    let ops = vec![
        SwapOperation {
            pool_addr: pool_a.clone(),
            offer_asset_info: TokenType::CreatorToken {
                denom: denom_a.clone(),
            },
            ask_asset_info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
        },
        SwapOperation {
            pool_addr: pool_b.clone(),
            offer_asset_info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            ask_asset_info: TokenType::CreatorToken {
                denom: denom_b.clone(),
            },
        },
    ];
    let route = |minimum_receive: u128| router::msg::ExecuteMsg::ExecuteMultiHop {
        operations: ops.clone(),
        minimum_receive: Uint128::new(minimum_receive),
        deadline: None,
        recipient: None,
    };
    // A DIRECT null-belief hop-0 call proves the gate's identity: pool_a
    // rejects it with the exact `belief_price is required` message.
    let direct_hop0 = wasm
        .execute(
            &pool_a,
            &simple_swap_msg(1_000_000, None),
            &[Coin::new(1_000_000u128, UOSMO)],
            &crosser_a,
        )
        .unwrap_err();
    assert!(
        direct_hop0.to_string().contains("belief_price is required"),
        "control: direct null-belief hop must hit the gate, got: {direct_hop0}"
    );
    app.increase_time(30);

    // The PENDING router routes null-belief per hop, so hop 0 hits that
    // same gate and the whole route is rejected. The router wraps the
    // underlying pool error as `Hop 0 on pool … failed`, so we assert on
    // the wrapper (the pool's belief message is masked by the router's
    // reply handler) — the point is that a pending proposal grants NO
    // exemption, proven by the route failing at hop 0 here versus
    // succeeding once applied (step 5).
    let err = wasm
        .execute(
            &router_addr,
            &route(1),
            &[Coin::new(1_000_000_000u128, denom_a.clone())],
            &crosser_a,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("Hop 0") && err.to_string().contains("failed"),
        "a PENDING router must not be exempt: the route must fail at hop 0, got: {err}"
    );

    // Apply before the window: refused with the prod 48h timelock.
    let err = wasm
        .execute(
            &factory_addr,
            &FactoryExecuteMsg::ApplyRouter {},
            &[],
            &admin,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("not yet effective"),
        "ApplyRouter before 48h must be refused, got: {err}"
    );

    app.increase_time(TIMELOCK_SECONDS + 1);
    wasm.execute(
        &factory_addr,
        &FactoryExecuteMsg::ApplyRouter {},
        &[],
        &admin,
    )
    .unwrap();
    let reg: pool_factory_interfaces::RegisteredRouterResponse = wasm
        .query(
            &factory_addr,
            &factory::query::QueryMsg::PoolFactoryQuery(
                pool_factory_interfaces::FactoryQueryMsg::RegisteredRouter {},
            ),
        )
        .unwrap();
    assert_eq!(
        reg.router.as_ref().map(|a| a.to_string()),
        Some(router_addr.clone()),
        "applied router registered"
    );

    // --- (5) The registered router routes creatorA → OSMO → creatorB
    // end to end: two REAL native-pool swaps in one tx, minimum_receive
    // enforced by the terminal AssertReceived. ---
    let b_before = balance(&bank, &crosser_a.address(), &denom_b);
    let min_receive = 1_000u128;
    wasm.execute(
        &router_addr,
        &route(min_receive),
        &[Coin::new(1_000_000_000u128, denom_a.clone())],
        &crosser_a,
    )
    .unwrap();
    let b_after = balance(&bank, &crosser_a.address(), &denom_b);
    let received = b_after.checked_sub(b_before).unwrap();
    assert!(
        received >= Uint128::new(min_receive),
        "two-hop route delivered ≥ minimum_receive (got {received})"
    );
    app.increase_time(30);

    // --- (6) An impossible minimum_receive reverts the WHOLE route: the
    // hop swaps that already executed roll back, the offered creatorA
    // comes back, nothing is stranded on the router. ---
    let a_before = balance(&bank, &crosser_a.address(), &denom_a);
    let err = wasm
        .execute(
            &router_addr,
            &route(u128::pow(10, 18)),
            &[Coin::new(1_000_000_000u128, denom_a.clone())],
            &crosser_a,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("minimum receive") || err.to_string().contains("Slippage"),
        "route must die on the AssertReceived floor, got: {err}"
    );
    assert_eq!(
        balance(&bank, &crosser_a.address(), &denom_a),
        a_before,
        "failed route returned the full creatorA input"
    );
    assert_eq!(
        balance(&bank, &router_addr, &denom_a),
        Uint128::zero(),
        "nothing stranded on the router (denom A)"
    );
    assert_eq!(
        balance(&bank, &router_addr, UOSMO),
        Uint128::zero(),
        "nothing stranded on the router (OSMO)"
    );
    app.increase_time(30);

    // --- (7) F-5 no-wedge: the guard set by the failed route rolled back
    // with the revert; the immediately following route succeeds. ---
    let b_before = balance(&bank, &crosser_a.address(), &denom_b);
    wasm.execute(
        &router_addr,
        &route(1),
        &[Coin::new(1_000_000_000u128, denom_a.clone())],
        &crosser_a,
    )
    .expect("route after a failed route — ROUTE_IN_PROGRESS must not wedge");
    assert!(
        balance(&bank, &crosser_a.address(), &denom_b) > b_before,
        "post-revert route delivered creatorB tokens"
    );
}
