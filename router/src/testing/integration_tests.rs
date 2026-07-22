//! Router integration tests.
//!
//! Stands up a `cw-multi-test` world with a handful of mock pools, then
//! exercises the router end to end. The mock pool implements just the
//! surface area the router needs (see [`crate::testing::mock_pool`]) which
//! keeps each test focused on router behaviour rather than factory +
//! oracle + threshold setup.
//!
//! Phase-1 migration note: the "creator token" is now a native Osmosis
//! TokenFactory bank denom (see `pool_factory_interfaces::asset::TokenType`),
//! NOT a CW20 contract. The harness therefore models each creator token as
//! a native bank denom (`factory/{creator}/ucreator`) held/seeded via the
//! bank module, and asserts balances via `bank_balance`. The router's
//! public `execute_multi_hop` currently only accepts a *native bluechip*
//! first hop (it rejects a `CreatorToken` first-hop offer), and the CW20
//! `Receive` entry point is a dead reject path — so routes whose FIRST hop
//! offers a creator token are not yet executable end to end and are marked
//! `#[ignore]` below.

use cosmwasm_std::testing::MockStorage;
use cosmwasm_std::{Addr, Coin, Empty, Timestamp, Uint128};
use cw_multi_test::{
    App, AppBuilder, BankKeeper, Contract, ContractWrapper, DistributionKeeper, Executor,
    FailingModule, GovFailingModule, IbcFailingModule, MockApiBech32, StakeKeeper, StargateFailing,
    WasmKeeper,
};
use pool_factory_interfaces::asset::TokenType;
use pool_factory_interfaces::routing::SwapOperation;

use crate::contract;
use crate::msg::{
    ConfigResponse, ExecuteMsg as RouterExecuteMsg, InstantiateMsg as RouterInstantiateMsg,
    QueryMsg as RouterQueryMsg, SimulateMultiHopResponse,
};
use crate::testing::{mock_factory, mock_pool};

const BLUECHIP_DENOM: &str = "ubluechip";
const POOL_RESERVE: u128 = 1_000_000;
const USER_NATIVE: u128 = 10_000_000;
/// Starting balance the user holds of each creator's native denom. (The
/// creator token is a TokenFactory bank denom post-migration.)
const USER_CREATOR: u128 = 1_000_000;

type TestApp = App<
    BankKeeper,
    MockApiBech32,
    MockStorage,
    FailingModule<Empty, Empty, Empty>,
    WasmKeeper<Empty, Empty>,
    StakeKeeper,
    DistributionKeeper,
    IbcFailingModule,
    GovFailingModule,
    StargateFailing,
>;

struct World {
    app: TestApp,
    user: Addr,
    admin: Addr,
    router: Addr,
    creator_a: Addr,
    creator_b: Addr,
    creator_c: Addr,
    pool_a: Addr,
    pool_b: Addr,
    pool_c: Addr,
    pool_uncommitted: Addr,
    pool_empty: Addr,
}

fn router_contract() -> Box<dyn Contract<Empty>> {
    Box::new(
        ContractWrapper::new(contract::execute, contract::instantiate, contract::query)
            .with_reply(contract::reply),
    )
}

fn mock_pool_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        mock_pool::execute,
        mock_pool::instantiate,
        mock_pool::query,
    ))
}

fn mock_factory_contract() -> Box<dyn Contract<Empty>> {
    Box::new(ContractWrapper::new(
        mock_factory::execute,
        mock_factory::instantiate,
        mock_factory::query,
    ))
}

/// The native TokenFactory denom modelling a creator token. Derived from
/// the creator's (deterministic) address so every construction site — the
/// pool pair, the factory registry, and the route ops — agrees on the same
/// denom string (which is what `TokenType::equal` compares).
fn creator_denom(creator: &Addr) -> String {
    format!("factory/{creator}/ucreator")
}

/// The `CreatorToken` `TokenType` for a given creator (a native denom now).
fn creator_token(creator: &Addr) -> TokenType {
    TokenType::CreatorToken {
        denom: creator_denom(creator),
    }
}

/// The canonical pair every mock pool in this harness wraps:
/// `[Native(bluechip), CreatorToken(creator)]` — matching `instantiate_pool`.
fn pool_pair(creator: &Addr) -> [TokenType; 2] {
    [
        TokenType::Native {
            denom: BLUECHIP_DENOM.to_string(),
        },
        creator_token(creator),
    ]
}

fn setup_world() -> World {
    let api = MockApiBech32::new("cosmwasm");
    let user = api.addr_make("user");
    let admin = api.addr_make("admin");

    // Creator tokens are native TokenFactory denoms now, so a creator is
    // just a (stable) address the denom is derived from — no CW20 contract
    // to instantiate. These addresses double as the denom seed and, in
    // `route_through_unregistered_pool_rejected`, as an unregistered target.
    let creator_a = api.addr_make("creator_a");
    let creator_b = api.addr_make("creator_b");
    let creator_c = api.addr_make("creator_c");
    let creator_uncommitted = api.addr_make("creator_uncommitted");
    let creator_empty = api.addr_make("creator_empty");

    let user_for_init = user.clone();
    let admin_for_init = admin.clone();
    let creators_for_init = [
        creator_a.clone(),
        creator_b.clone(),
        creator_c.clone(),
        creator_uncommitted.clone(),
        creator_empty.clone(),
    ];
    let mut app: TestApp = AppBuilder::new()
        .with_api(api)
        .build(|router, _api, storage| {
            // User: bluechip to spend, plus a balance of every creator denom
            // (each creator token is a native bank denom now).
            let mut user_coins = vec![Coin::new(USER_NATIVE, BLUECHIP_DENOM)];
            for c in &creators_for_init {
                user_coins.push(Coin::new(USER_CREATOR, creator_denom(c)));
            }
            router
                .bank
                .init_balance(storage, &user_for_init, user_coins)
                .unwrap();

            // Admin: bluechip + every creator denom, used to seed pool reserves.
            let mut admin_coins = vec![Coin::new(20 * POOL_RESERVE, BLUECHIP_DENOM)];
            for c in &creators_for_init {
                admin_coins.push(Coin::new(2 * POOL_RESERVE, creator_denom(c)));
            }
            router
                .bank
                .init_balance(storage, &admin_for_init, admin_coins)
                .unwrap();
        });

    let pool_code = app.store_code(mock_pool_contract());
    let factory_code = app.store_code(mock_factory_contract());
    let router_code = app.store_code(router_contract());

    let pool_a = instantiate_pool(&mut app, pool_code, &admin, &creator_a, true, true);
    let pool_b = instantiate_pool(&mut app, pool_code, &admin, &creator_b, true, true);
    let pool_c = instantiate_pool(&mut app, pool_code, &admin, &creator_c, true, true);
    let pool_uncommitted = instantiate_pool(
        &mut app,
        pool_code,
        &admin,
        &creator_uncommitted,
        false,
        true,
    );
    let pool_empty = instantiate_pool(&mut app, pool_code, &admin, &creator_empty, true, false);

    // Stand up the mock factory with every pool registered against its
    // canonical pair, then point the router at it. The router queries this
    // for `PoolByAddress` on each hop, so an unregistered address is
    // refused before any funds move.
    let factory = app
        .instantiate_contract(
            factory_code,
            admin.clone(),
            &mock_factory::InstantiateMsg {
                pools: vec![
                    mock_factory::RegistryEntry {
                        pool_addr: pool_a.to_string(),
                        pool_token_info: pool_pair(&creator_a),
                    },
                    mock_factory::RegistryEntry {
                        pool_addr: pool_b.to_string(),
                        pool_token_info: pool_pair(&creator_b),
                    },
                    mock_factory::RegistryEntry {
                        pool_addr: pool_c.to_string(),
                        pool_token_info: pool_pair(&creator_c),
                    },
                    mock_factory::RegistryEntry {
                        pool_addr: pool_uncommitted.to_string(),
                        pool_token_info: pool_pair(&creator_uncommitted),
                    },
                    mock_factory::RegistryEntry {
                        pool_addr: pool_empty.to_string(),
                        pool_token_info: pool_pair(&creator_empty),
                    },
                ],
            },
            &[],
            "mock_factory",
            None,
        )
        .unwrap();

    let router = app
        .instantiate_contract(
            router_code,
            admin.clone(),
            &RouterInstantiateMsg {
                factory_addr: factory.to_string(),
                bluechip_denom: BLUECHIP_DENOM.to_string(),
                admin: admin.to_string(),
            },
            &[],
            "router",
            None,
        )
        .unwrap();

    World {
        app,
        user,
        admin,
        router,
        creator_a,
        creator_b,
        creator_c,
        pool_a,
        pool_b,
        pool_c,
        pool_uncommitted,
        pool_empty,
    }
}

fn instantiate_pool(
    app: &mut TestApp,
    code_id: u64,
    admin: &Addr,
    creator: &Addr,
    fully_committed: bool,
    seed_reserves: bool,
) -> Addr {
    let pool = app
        .instantiate_contract(
            code_id,
            admin.clone(),
            &mock_pool::InstantiateMsg {
                asset_infos: [
                    TokenType::Native {
                        denom: BLUECHIP_DENOM.to_string(),
                    },
                    creator_token(creator),
                ],
                fully_committed,
            },
            &[],
            "mock_pool",
            None,
        )
        .unwrap();
    if seed_reserves {
        // Both reserves are native bank balances now: bluechip AND the
        // creator TokenFactory denom (previously the creator side was seeded
        // via a CW20 `Transfer`).
        app.send_tokens(
            admin.clone(),
            pool.clone(),
            &[Coin::new(POOL_RESERVE, BLUECHIP_DENOM)],
        )
        .unwrap();
        app.send_tokens(
            admin.clone(),
            pool.clone(),
            &[Coin::new(POOL_RESERVE, creator_denom(creator))],
        )
        .unwrap();
    }
    pool
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

fn bank_balance(app: &TestApp, account: &Addr, denom: &str) -> Uint128 {
    app.wrap().query_balance(account, denom).unwrap().amount
}

/// Convenience: an account's balance of a creator's native denom.
fn creator_balance(app: &TestApp, account: &Addr, creator: &Addr) -> Uint128 {
    bank_balance(app, account, &creator_denom(creator))
}

fn op(pool: &Addr, offer: TokenType, ask: TokenType) -> SwapOperation {
    SwapOperation {
        pool_addr: pool.to_string(),
        offer_asset_info: offer,
        ask_asset_info: ask,
    }
}

// ---------------------------------------------------------------------------
// Test cases
// ---------------------------------------------------------------------------

// Creator->bluechip->creator route: the first hop offers the creator token,
// now a native TokenFactory denom attached as funds and accepted by
// `execute_multi_hop` through the standard native offer path.
#[test]
fn happy_path_two_hop_creator_to_creator() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let creator_b = creator_token(&world.creator_b);

    let route = vec![
        op(&world.pool_a, creator_a.clone(), bluechip.clone()),
        op(&world.pool_b, bluechip.clone(), creator_b.clone()),
    ];

    let amount = Uint128::new(100_000);
    let creator_b_before = creator_balance(&world.app, &world.user, &world.creator_b);

    // Post-migration a creator-token offer is native funds attached to a
    // plain `ExecuteMultiHop` (previously a `cw20::Send` to the router).
    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(amount.u128(), creator_denom(&world.creator_a))],
        )
        .unwrap();

    let creator_b_after = creator_balance(&world.app, &world.user, &world.creator_b);
    assert!(
        creator_b_after > creator_b_before,
        "user should receive creator B"
    );

    // Router holds zero of every involved token after a successful route.
    assert_eq!(
        creator_balance(&world.app, &world.router, &world.creator_a),
        Uint128::zero()
    );
    assert_eq!(
        creator_balance(&world.app, &world.router, &world.creator_b),
        Uint128::zero()
    );
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );
}

#[test]
fn single_hop_native_passthrough() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let route = vec![op(&world.pool_a, bluechip.clone(), creator_a)];

    let amount = Uint128::new(50_000);
    let creator_a_before = creator_balance(&world.app, &world.user, &world.creator_a);

    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(amount.u128(), BLUECHIP_DENOM)],
        )
        .unwrap();

    let creator_a_after = creator_balance(&world.app, &world.user, &world.creator_a);
    assert!(creator_a_after > creator_a_before);
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );
    assert_eq!(
        creator_balance(&world.app, &world.router, &world.creator_a),
        Uint128::zero()
    );
}

#[test]
fn route_through_unregistered_pool_rejected() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);

    // Point the hop at an address that is NOT a registered pool — the shape
    // a malicious frontend would use to steer funds to a contract it
    // controls. Without registry validation the router would forward the
    // user's bluechip to it; the registry check must refuse before any funds
    // move. (The creator addr is a convenient unregistered address here.)
    let rogue_pool = world.creator_a.clone();
    let route = vec![op(&rogue_pool, bluechip, creator_a)];

    let amount = Uint128::new(50_000);
    let user_before = bank_balance(&world.app, &world.user, BLUECHIP_DENOM);

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(amount.u128(), BLUECHIP_DENOM)],
        )
        .unwrap_err();

    assert!(
        err.root_cause().to_string().contains("not registered"),
        "expected PoolNotRegistered, got: {err}"
    );

    // The whole tx reverted atomically: the user's bluechip is untouched
    // and nothing is stranded in the router.
    assert_eq!(
        bank_balance(&world.app, &world.user, BLUECHIP_DENOM),
        user_before
    );
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );
}

#[test]
fn route_with_mislabeled_pair_rejected() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    // Ask for creator B out of pool A, whose registered pair is
    // [bluechip, creator A]. The hop targets a genuine, registered pool
    // but declares a side that pool does not trade — rejected by the
    // pair-match half of the registry check before any funds move.
    let creator_b = creator_token(&world.creator_b);
    let route = vec![op(&world.pool_a, bluechip, creator_b)];

    let amount = Uint128::new(50_000);
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(amount.u128(), BLUECHIP_DENOM)],
        )
        .unwrap_err();

    assert!(
        err.root_cause()
            .to_string()
            .contains("not this pool's pair"),
        "expected HopPairMismatch, got: {err}"
    );
}

#[test]
fn slippage_exceeded_reverts_route() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let route = vec![op(&world.pool_a, bluechip.clone(), creator_a)];

    let amount = Uint128::new(50_000);
    let user_before = bank_balance(&world.app, &world.user, BLUECHIP_DENOM);

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                // Demand absurdly more than the pool can possibly return.
                minimum_receive: Uint128::new(u128::MAX / 2),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(amount.u128(), BLUECHIP_DENOM)],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("Slippage exceeded"),
        "expected SlippageExceeded, got: {err}"
    );

    // Reverted: user balance unchanged, router holds nothing.
    let user_after = bank_balance(&world.app, &world.user, BLUECHIP_DENOM);
    assert_eq!(user_before, user_after);
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );
}

#[test]
fn max_hops_exceeded_rejected() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let creator_b = creator_token(&world.creator_b);
    let creator_c = creator_token(&world.creator_c);
    // Four hops: bluechip -> A -> bluechip -> B -> bluechip... exceed MAX_HOPS=3.
    let route = vec![
        op(&world.pool_a, bluechip.clone(), creator_a.clone()),
        op(&world.pool_a, creator_a.clone(), bluechip.clone()),
        op(&world.pool_b, bluechip.clone(), creator_b.clone()),
        op(&world.pool_c, creator_b.clone(), creator_c.clone()),
    ];

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(10_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("maximum of 3 hops"),
        "expected MaxHopsExceeded, got: {err}"
    );
}

#[test]
fn deadline_expired_rejected() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let route = vec![op(&world.pool_a, bluechip.clone(), creator_a)];

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: Some(Timestamp::from_seconds(1)),
                recipient: None,
            },
            &[Coin::new(10_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("deadline exceeded"),
        "expected DeadlineExceeded, got: {err}"
    );
}

#[test]
fn same_input_output_rejected() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    // bluechip -> A -> bluechip: structurally a round trip.
    let route = vec![
        op(&world.pool_a, bluechip.clone(), creator_a.clone()),
        op(&world.pool_a, creator_a, bluechip.clone()),
    ];

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(10_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("input and final output must differ"),
        "expected SameInputOutput, got: {err}"
    );
}

#[test]
fn zero_liquidity_pool_in_path_errors_with_hop_context() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    // pool_empty was instantiated with seed_reserves=false, so its
    // bluechip and creator-denom balances are both zero.
    let creator_empty: TokenType = {
        // Read the pool's pair to get the creator denom it knows about.
        let pair: mock_pool::PairResponse = world
            .app
            .wrap()
            .query_wasm_smart(&world.pool_empty, &mock_pool::QueryMsg::Pair {})
            .unwrap();
        match &pair.asset_infos[1] {
            TokenType::CreatorToken { denom } => TokenType::CreatorToken {
                denom: denom.clone(),
            },
            _ => panic!("expected creator token on side 1"),
        }
    };
    let route = vec![op(&world.pool_empty, bluechip.clone(), creator_empty)];

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(10_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    let msg = err.root_cause().to_string();
    assert!(
        msg.contains("Hop 0") && msg.contains("no liquidity"),
        "expected HopFailed with hop context, got: {msg}"
    );
}

#[test]
fn router_holds_zero_after_successful_route() {
    // Same flow as the happy path but explicitly verifies the router's
    // balance for every involved asset both BEFORE and AFTER the route.
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let creator_b = creator_token(&world.creator_b);
    let route = vec![
        op(&world.pool_a, creator_a.clone(), bluechip.clone()),
        op(&world.pool_b, bluechip.clone(), creator_b.clone()),
    ];

    for creator in [&world.creator_a, &world.creator_b] {
        assert_eq!(
            creator_balance(&world.app, &world.router, creator),
            Uint128::zero()
        );
    }
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );

    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(100_000u128, creator_denom(&world.creator_a))],
        )
        .unwrap();

    for creator in [&world.creator_a, &world.creator_b] {
        assert_eq!(
            creator_balance(&world.app, &world.router, creator),
            Uint128::zero(),
            "router still holds creator denom after route",
        );
    }
    assert_eq!(
        bank_balance(&world.app, &world.router, BLUECHIP_DENOM),
        Uint128::zero()
    );
}

#[test]
fn simulate_matches_execute() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let creator_b = creator_token(&world.creator_b);
    let route = vec![
        op(&world.pool_a, creator_a.clone(), bluechip.clone()),
        op(&world.pool_b, bluechip.clone(), creator_b.clone()),
    ];
    let offer_amount = Uint128::new(100_000);

    let sim: SimulateMultiHopResponse = world
        .app
        .wrap()
        .query_wasm_smart(
            &world.router,
            &RouterQueryMsg::SimulateMultiHop {
                operations: route.clone(),
                offer_amount,
            },
        )
        .unwrap();
    assert_eq!(sim.intermediate_amounts.len(), 2);
    assert_eq!(sim.final_amount, *sim.intermediate_amounts.last().unwrap());

    let creator_b_before = creator_balance(&world.app, &world.user, &world.creator_b);
    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(offer_amount.u128(), creator_denom(&world.creator_a))],
        )
        .unwrap();
    let creator_b_after = creator_balance(&world.app, &world.user, &world.creator_b);
    let actual_received = creator_b_after - creator_b_before;
    assert_eq!(
        actual_received, sim.final_amount,
        "execute output should match simulation exactly"
    );
}

#[test]
fn commit_phase_pool_rejected_in_simulation() {
    let world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_uncommitted: TokenType = {
        let pair: mock_pool::PairResponse = world
            .app
            .wrap()
            .query_wasm_smart(&world.pool_uncommitted, &mock_pool::QueryMsg::Pair {})
            .unwrap();
        match &pair.asset_infos[1] {
            TokenType::CreatorToken { denom } => TokenType::CreatorToken {
                denom: denom.clone(),
            },
            _ => panic!("expected creator token on side 1"),
        }
    };
    let route = vec![op(
        &world.pool_uncommitted,
        bluechip.clone(),
        creator_uncommitted,
    )];

    let err = world
        .app
        .wrap()
        .query_wasm_smart::<SimulateMultiHopResponse>(
            &world.router,
            &RouterQueryMsg::SimulateMultiHop {
                operations: route,
                offer_amount: Uint128::new(10_000),
            },
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("commit phase"),
        "expected PoolInCommitPhase error from simulation, got: {msg}"
    );
}

#[test]
fn commit_phase_pool_rejected_in_execution() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_uncommitted: TokenType = {
        let pair: mock_pool::PairResponse = world
            .app
            .wrap()
            .query_wasm_smart(&world.pool_uncommitted, &mock_pool::QueryMsg::Pair {})
            .unwrap();
        match &pair.asset_infos[1] {
            TokenType::CreatorToken { denom } => TokenType::CreatorToken {
                denom: denom.clone(),
            },
            _ => panic!("expected creator token on side 1"),
        }
    };
    let route = vec![op(
        &world.pool_uncommitted,
        bluechip.clone(),
        creator_uncommitted,
    )];

    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route,
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(10_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    let msg = err.root_cause().to_string();
    // L-02 — execution now rejects a pre-threshold pool UP FRONT with the
    // same actionable `PoolInCommitPhase` error the simulation path returns,
    // instead of dispatching the hop and surfacing an opaque wrapped
    // `HopFailed` from the pool's swap rejection. Keeps simulate and execute
    // in agreement.
    assert!(
        msg.contains("hop 0") && msg.contains("commit phase"),
        "expected PoolInCommitPhase early rejection, got: {msg}"
    );
}

#[test]
fn propose_config_update_admin_only() {
    let mut world = setup_world();

    // Non-admin propose: rejected.
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    // Admin propose: succeeds.
    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();

    // Live config is unchanged at this point — only PENDING_CONFIG was
    // written. Admin can still configure further (proposal hasn't applied).
    let cfg: ConfigResponse = world
        .app
        .wrap()
        .query_wasm_smart(&world.router, &RouterQueryMsg::Config {})
        .unwrap();
    assert_eq!(cfg.admin, world.admin);
}

#[test]
fn update_config_before_timelock_rejected() {
    let mut world = setup_world();

    // Propose now.
    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();

    // Try to apply immediately (no time advance): must reject with
    // TimelockNotExpired.
    let err = world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::UpdateConfig {},
            &[],
        )
        .unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("timelock not expired"),
        "got: {err}"
    );

    // Live config still unchanged.
    let cfg: ConfigResponse = world
        .app
        .wrap()
        .query_wasm_smart(&world.router, &RouterQueryMsg::Config {})
        .unwrap();
    assert_eq!(cfg.admin, world.admin);
}

#[test]
fn update_config_applies_after_timelock() {
    let mut world = setup_world();

    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();

    // Advance past the 48h timelock + 1s buffer.
    world.app.update_block(|block| {
        block.time = block
            .time
            .plus_seconds(crate::state::ROUTER_TIMELOCK_SECONDS + 1);
    });

    // Non-admin apply: still rejected (auth check is on apply too).
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::UpdateConfig {},
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    // Admin apply: succeeds, live config updates, pending clears.
    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::UpdateConfig {},
            &[],
        )
        .unwrap();

    let cfg: ConfigResponse = world
        .app
        .wrap()
        .query_wasm_smart(&world.router, &RouterQueryMsg::Config {})
        .unwrap();
    assert_eq!(cfg.admin, world.user);
    assert_eq!(cfg.bluechip_denom, BLUECHIP_DENOM);
}

#[test]
fn re_propose_while_pending_rejected() {
    let mut world = setup_world();

    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();

    // Re-propose while a prior proposal is still pending: rejected. The
    // admin must explicitly cancel before re-proposing.
    let err = world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.admin.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("already pending"),
        "got: {err}"
    );
}

#[test]
fn cancel_config_update_clears_pending() {
    let mut world = setup_world();

    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.user.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();

    // Non-admin cancel: rejected.
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::CancelConfigUpdate {},
            &[],
        )
        .unwrap_err();
    assert!(err.root_cause().to_string().contains("Unauthorized"));

    // Admin cancel: succeeds.
    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::CancelConfigUpdate {},
            &[],
        )
        .unwrap();

    // Cancel again: NoPendingConfigUpdate.
    let err = world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::CancelConfigUpdate {},
            &[],
        )
        .unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("No pending config update"),
        "got: {err}"
    );

    // After cancel, a fresh propose works (the gate is per-pending, not
    // a one-shot).
    world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ProposeConfigUpdate {
                admin: Some(world.admin.to_string()),
                factory_addr: None,
            },
            &[],
        )
        .unwrap();
}

#[test]
fn apply_with_no_pending_rejected() {
    let mut world = setup_world();
    let err = world
        .app
        .execute_contract(
            world.admin.clone(),
            world.router.clone(),
            &RouterExecuteMsg::UpdateConfig {},
            &[],
        )
        .unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("No pending config update"),
        "got: {err}"
    );
}

/// Asserts the per-hop `max_spread` the router forwards to pools is
/// pinned to the 5% hard cap.
#[test]
fn router_forwards_hard_cap_max_spread_per_hop() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: vec![op(&world.pool_a, bluechip.clone(), creator_a.clone())],
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(100_000u128, BLUECHIP_DENOM)],
        )
        .unwrap();

    let forwarded: Option<cosmwasm_std::Decimal> = world
        .app
        .wrap()
        .query_wasm_smart(&world.pool_a, &mock_pool::QueryMsg::LastMaxSpread {})
        .unwrap();
    assert_eq!(forwarded, Some(cosmwasm_std::Decimal::percent(5)));
}

/// Simulation mirrors execution's registry validation: a route
/// through an address the factory doesn't know is rejected up front
/// instead of producing garbage (or a confusing pool-side error).
#[test]
fn simulation_rejects_unregistered_pool() {
    let world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let err = world
        .app
        .wrap()
        .query_wasm_smart::<SimulateMultiHopResponse>(
            &world.router,
            &RouterQueryMsg::SimulateMultiHop {
                operations: vec![op(
                    &Addr::unchecked("cosmwasm1notarealpoolnotarealpoolnotareal"),
                    bluechip,
                    creator_a,
                )],
                offer_amount: Uint128::new(100_000),
            },
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("not registered"),
        "expected registry rejection, got: {err}"
    );
}

/// minimum_receive is the only end-to-end slippage protection (per-hop
/// gates are pinned to the pools' 5% hard cap), so a zero value — i.e.
/// no protection at all — is rejected at the shared entry point.
#[test]
fn router_rejects_zero_minimum_receive() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);

    // Native-offered path.
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: vec![op(&world.pool_a, bluechip.clone(), creator_a.clone())],
                minimum_receive: Uint128::zero(),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(100_000u128, BLUECHIP_DENOM)],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("minimum_receive"),
        "expected zero-minimum rejection, got: {err:?}"
    );

    // TODO(phase1-migration): the second leg of this test used to exercise
    // the CW20-offered path (`cw20::Send` -> `execute_receive_cw20`) hitting
    // the same shared zero-minimum gate. Post-migration the creator token is
    // a native denom and that CW20 entry is a dead reject path; a creator
    // first-hop native offer is likewise rejected by `execute_multi_hop`
    // before the shared gate is reached, so there is no longer a second
    // offer path that reaches the zero-minimum check. Re-add coverage once
    // creator-token first-hop offers are wired.
}

/// F-5 — a route that FAILS must leave `ROUTE_IN_PROGRESS` clear so the next
/// route still works. The guard is set very early in `start_multi_hop`; if a
/// failure did not roll it back, the router would wedge permanently after the
/// first failed swap. Here the first route fails at the final slippage assert
/// (absurd `minimum_receive`), then an identical well-priced route succeeds.
#[test]
fn failed_route_does_not_wedge_the_reentrancy_guard() {
    let mut world = setup_world();

    let bluechip = TokenType::Native {
        denom: BLUECHIP_DENOM.to_string(),
    };
    let creator_a = creator_token(&world.creator_a);
    let creator_b = creator_token(&world.creator_b);
    let route = || {
        vec![
            op(&world.pool_a, creator_a.clone(), bluechip.clone()),
            op(&world.pool_b, bluechip.clone(), creator_b.clone()),
        ]
    };

    // 1. Route fails at the final slippage check (minimum_receive unattainable).
    let err = world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route(),
                minimum_receive: Uint128::new(100_000_000),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(100_000u128, creator_denom(&world.creator_a))],
        )
        .unwrap_err();
    assert!(
        err.root_cause().to_string().contains("Slippage"),
        "expected the first route to fail on slippage, got: {err:?}"
    );

    // 2. An identical, well-priced route must now succeed — proving the guard
    // was rolled back with the failed tx, not left latched.
    world
        .app
        .execute_contract(
            world.user.clone(),
            world.router.clone(),
            &RouterExecuteMsg::ExecuteMultiHop {
                operations: route(),
                minimum_receive: Uint128::new(1),
                deadline: None,
                recipient: None,
            },
            &[Coin::new(100_000u128, creator_denom(&world.creator_a))],
        )
        .expect("a valid route after a failed one must succeed (guard not wedged)");
}
