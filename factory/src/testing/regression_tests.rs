use cosmwasm_std::testing::{
    message_info, mock_env, MockApi, MockQuerier, MockStorage, MOCK_CONTRACT_ADDR,
};
use cosmwasm_std::{
    Addr, Binary, Coin, Decimal, Empty, Event, OwnedDeps, Reply, SubMsgResponse, SubMsgResult,
    Uint128,
};

use crate::asset::TokenType;
use crate::error::ContractError;
use crate::execute::{
    encode_reply_id, execute, instantiate, pool_creation_reply, FINALIZE_POOL, MINT_CREATE_POOL,
    SET_TOKENS,
};
use crate::mock_querier::WasmMockQuerier;
use crate::msg::{CreatorTokenInfo, ExecuteMsg};
use crate::pool_struct::{CreatePool, PoolConfigUpdate, PoolDetails};
use crate::state::{
    FactoryInstantiate, PENDING_CONFIG, POOLS_BY_CONTRACT_ADDRESS, POOLS_BY_ID, POOL_COUNTER,
    POOL_ID_BY_ADDRESS, POOL_THRESHOLD_CROSSED,
};

fn make_addr(label: &str) -> Addr {
    MockApi::default().addr_make(label)
}

fn admin_addr() -> Addr {
    make_addr("admin")
}

fn mock_deps_with_querier(
    contract_balance: &[Coin],
) -> OwnedDeps<MockStorage, MockApi, WasmMockQuerier> {
    let custom_querier: WasmMockQuerier =
        WasmMockQuerier::new(MockQuerier::new(&[(MOCK_CONTRACT_ADDR, contract_balance)]));

    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: custom_querier,
        custom_query_type: Default::default(),
    }
}

fn default_factory_config() -> FactoryInstantiate {
    FactoryInstantiate {
        cw721_nft_contract_id: 58,
        factory_admin_address: admin_addr(),
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: make_addr("ubluechip"),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 14,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: Uint128::new(1_000_000),
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    }
}

fn setup_factory(deps: &mut OwnedDeps<MockStorage, MockApi, WasmMockQuerier>) {
    let config = default_factory_config();
    let env = mock_env();
    let info = message_info(&admin_addr(), &[]);
    instantiate(deps.as_mut(), env, info, config).unwrap();
}

/// Funds covering the flat native commit-pool creation fee configured in
/// `default_factory_config` (`pool_creation_fee = 1_000_000`).
fn creation_fee_funds() -> [Coin; 1] {
    [Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000),
    }]
}

/// Save a minimal `PoolDetails` for `pool_id` so production code that looks
/// up a pool address via `POOLS_BY_ID.load(..).creator_pool_addr` works in
/// tests. Also writes the `POOL_ID_BY_ADDRESS` reverse index (mirroring
/// `state::register_pool`) so handlers that resolve pools by address
/// (e.g. `lookup_pool_by_addr` in `crate::state`) find the fixture.
fn register_test_pool_addr(
    storage: &mut dyn cosmwasm_std::Storage,
    pool_id: u64,
    pool_addr: &Addr,
) {
    POOLS_BY_ID
        .save(
            storage,
            pool_id,
            &PoolDetails {
                pool_id,
                pool_token_info: [
                    TokenType::Native {
                        denom: "ubluechip".to_string(),
                    },
                    TokenType::CreatorToken {
                        contract_addr: Addr::unchecked("token"),
                    },
                ],
                creator_pool_addr: pool_addr.clone(),
            },
        )
        .unwrap();
    POOL_ID_BY_ADDRESS
        .save(storage, pool_addr.clone(), &pool_id)
        .unwrap();
}

#[allow(deprecated)]
fn create_instantiate_reply(id: u64, contract_addr: &str) -> Reply {
    Reply {
        id,
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![
                Event::new("instantiate").add_attribute("_contract_address", contract_addr)
            ],
            msg_responses: vec![],
            data: None,
        }),
        gas_used: 0,
        payload: Binary::default(),
    }
}

#[test]
fn test_notify_threshold_crossed_unauthorized_caller() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Register pool 1 at a specific address
    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_contract_1"));

    let env = mock_env();

    // A random address tries to notify - should fail
    let hacker_info = message_info(&Addr::unchecked("hacker"), &[]);
    let msg = ExecuteMsg::NotifyThresholdCrossed {
        pool_id: 1,
        crossed_at: None,
    };

    let err = execute(deps.as_mut(), env, hacker_info, msg).unwrap_err();
    assert!(
        err.to_string()
            .contains("Only the registered pool contract"),
        "Expected pool authorization error, got: {}",
        err
    );
}

#[test]
fn test_notify_threshold_crossed_double_call_prevention() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Register pool 1
    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_contract_1"));

    // Mark the crossing as already recorded
    POOL_THRESHOLD_CROSSED
        .save(&mut deps.storage, 1, &true)
        .unwrap();

    let env = mock_env();
    let pool_info = message_info(&Addr::unchecked("pool_contract_1"), &[]);
    let msg = ExecuteMsg::NotifyThresholdCrossed {
        pool_id: 1,
        crossed_at: None,
    };

    let err = execute(deps.as_mut(), env, pool_info, msg).unwrap_err();
    assert!(
        err.to_string()
            .contains("Threshold crossing already recorded for this pool"),
        "Expected idempotency-gate error, got: {}",
        err
    );
}

/// Success path: NotifyThresholdCrossed is a pure registry recording.
/// The response carries NO messages (no mint, no ordinal allocation) and
/// the attrs action=threshold_crossed / pool_id / crossed_at.
#[test]
fn test_notify_threshold_crossed_records_flag_with_no_messages() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_contract_1"));

    let env = mock_env();
    let crossed_at = env.block.time.minus_seconds(42);
    let pool_info = message_info(&Addr::unchecked("pool_contract_1"), &[]);
    let msg = ExecuteMsg::NotifyThresholdCrossed {
        pool_id: 1,
        crossed_at: Some(crossed_at),
    };

    let res = execute(deps.as_mut(), env, pool_info, msg).unwrap();
    assert!(
        res.messages.is_empty(),
        "threshold-cross recording must emit no messages; got: {:?}",
        res.messages
    );
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "action" && a.value == "threshold_crossed"));
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "pool_id" && a.value == "1"));
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "crossed_at" && a.value == crossed_at.to_string()));

    assert!(POOL_THRESHOLD_CROSSED.load(&deps.storage, 1).unwrap());
}

#[test]
fn test_notify_threshold_crossed_unregistered_pool() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Don't register any pool in POOLS_BY_ID

    let env = mock_env();
    let pool_info = message_info(&Addr::unchecked("pool_contract_1"), &[]);
    let msg = ExecuteMsg::NotifyThresholdCrossed {
        pool_id: 999,
        crossed_at: None,
    };

    let err = execute(deps.as_mut(), env, pool_info, msg).unwrap_err();
    assert!(
        err.to_string().contains("not found in registry"),
        "Expected registry error, got: {}",
        err
    );
}

#[test]
fn test_cancel_config_update() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    // Propose a config update first
    let new_config = default_factory_config();
    let propose_msg = ExecuteMsg::ProposeConfigUpdate { config: new_config };
    execute(deps.as_mut(), env.clone(), admin_info.clone(), propose_msg).unwrap();

    // Verify pending config exists
    assert!(PENDING_CONFIG.may_load(&deps.storage).unwrap().is_some());

    // Cancel it
    let cancel_msg = ExecuteMsg::CancelConfigUpdate {};
    let res = execute(deps.as_mut(), env, admin_info, cancel_msg).unwrap();

    assert!(res
        .attributes
        .iter()
        .any(|a| a.value == "cancel_config_update"));

    // Pending config should be gone
    assert!(PENDING_CONFIG.may_load(&deps.storage).unwrap().is_none());
}

#[test]
fn test_cancel_config_update_unauthorized() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    // Propose
    let propose_msg = ExecuteMsg::ProposeConfigUpdate {
        config: default_factory_config(),
    };
    execute(deps.as_mut(), env.clone(), admin_info, propose_msg).unwrap();

    // Non-admin tries to cancel
    let hacker_info = message_info(&Addr::unchecked("hacker"), &[]);
    let cancel_msg = ExecuteMsg::CancelConfigUpdate {};
    let err = execute(deps.as_mut(), env, hacker_info, cancel_msg).unwrap_err();
    assert!(matches!(err, ContractError::Unauthorized {}));
}

#[test]
fn test_config_update_before_timelock_fails() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let mut env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    // Propose config update
    let propose_msg = ExecuteMsg::ProposeConfigUpdate {
        config: default_factory_config(),
    };
    execute(deps.as_mut(), env.clone(), admin_info.clone(), propose_msg).unwrap();

    // Try to execute immediately (before 48h timelock)
    let update_msg = ExecuteMsg::UpdateConfig {};
    let err = execute(
        deps.as_mut(),
        env.clone(),
        admin_info.clone(),
        update_msg.clone(),
    )
    .unwrap_err();

    match err {
        ContractError::TimelockNotExpired { effective_after } => {
            assert!(effective_after > env.block.time);
        }
        _ => panic!("Expected TimelockNotExpired, got: {}", err),
    }

    // Advance time past the admin timelock
    env.block.time = env
        .block
        .time
        .plus_seconds(crate::state::ADMIN_TIMELOCK_SECONDS + 1);
    let res = execute(deps.as_mut(), env, admin_info, update_msg).unwrap();
    assert!(res
        .attributes
        .iter()
        .any(|a| a.value == "execute_update_config"));
}

#[test]
fn test_update_pool_config_sends_message_to_pool() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Register a pool
    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_contract_1"));

    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    let update = PoolConfigUpdate {
        lp_fee: Some(Decimal::percent(5)),
        min_commit_interval: Some(120),
        ..Default::default()
    };

    // Step 1: Propose — no messages sent yet
    let msg = ExecuteMsg::ProposePoolConfigUpdate {
        pool_id: 1,
        pool_config: update,
    };
    let res = execute(deps.as_mut(), env.clone(), admin_info.clone(), msg).unwrap();
    assert_eq!(res.messages.len(), 0);
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "pool_id" && a.value == "1"));

    // Step 2: Execute after timelock — should send WasmMsg to pool
    let mut future_env = env;
    future_env.block.time = future_env
        .block
        .time
        .plus_seconds(crate::state::ADMIN_TIMELOCK_SECONDS + 1);
    let apply_msg = ExecuteMsg::ExecutePoolConfigUpdate { pool_id: 1 };
    let res = execute(deps.as_mut(), future_env, admin_info, apply_msg).unwrap();
    assert_eq!(res.messages.len(), 1);
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "pool_id" && a.value == "1"));
}

#[test]
fn test_update_pool_config_unauthorized() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_contract_1"));

    let env = mock_env();
    let hacker_info = message_info(&Addr::unchecked("hacker"), &[]);

    let update = PoolConfigUpdate {
        lp_fee: Some(Decimal::percent(5)),
        min_commit_interval: None,
        ..Default::default()
    };

    let msg = ExecuteMsg::ProposePoolConfigUpdate {
        pool_id: 1,
        pool_config: update,
    };

    let err = execute(deps.as_mut(), env, hacker_info, msg).unwrap_err();
    assert!(matches!(err, ContractError::Unauthorized {}));
}

#[test]
fn test_update_pool_config_nonexistent_pool() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Don't register pool 99
    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    let update = PoolConfigUpdate {
        lp_fee: None,
        min_commit_interval: None,
        ..Default::default()
    };

    let msg = ExecuteMsg::ProposePoolConfigUpdate {
        pool_id: 99,
        pool_config: update,
    };

    let err = execute(deps.as_mut(), env, admin_info, msg).unwrap_err();
    // Pool 99 not found in registry
    assert!(
        err.to_string().contains("not found") || err.to_string().contains("type: cw_storage_plus")
    );
}

/// — propose-time bounds check for the
/// `min_commit_usd_pre_threshold` /
/// `min_commit_usd_post_threshold` knobs.
#[test]
fn test_propose_pool_config_commit_floor_bounds() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);
    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &[]);

    // Register a Commit pool (id=1).
    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("commit_pool_1"));

    // Zero floor is rejected.
    let zero = PoolConfigUpdate {
        min_commit_usd_pre_threshold: Some(Uint128::zero()),
        ..Default::default()
    };
    let err = execute(
        deps.as_mut(),
        env.clone(),
        admin_info.clone(),
        ExecuteMsg::ProposePoolConfigUpdate {
            pool_id: 1,
            pool_config: zero,
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("must be non-zero"),
        "expected non-zero rejection, got: {}",
        err
    );

    // Above-cap floor is rejected.
    let too_high = PoolConfigUpdate {
        min_commit_usd_post_threshold: Some(
            crate::pool_struct::POOL_CONFIG_MAX_MIN_COMMIT_USD + Uint128::new(1),
        ),
        ..Default::default()
    };
    let err = execute(
        deps.as_mut(),
        env.clone(),
        admin_info.clone(),
        ExecuteMsg::ProposePoolConfigUpdate {
            pool_id: 1,
            pool_config: too_high,
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("exceeds maximum"),
        "expected ceiling rejection, got: {}",
        err
    );

    // Same valid floor against the commit pool is accepted.
    let ok = PoolConfigUpdate {
        min_commit_usd_pre_threshold: Some(Uint128::new(10_000_000)),
        min_commit_usd_post_threshold: Some(Uint128::new(2_000_000)),
        ..Default::default()
    };
    execute(
        deps.as_mut(),
        env,
        admin_info,
        ExecuteMsg::ProposePoolConfigUpdate {
            pool_id: 1,
            pool_config: ok,
        },
    )
    .expect("commit pool should accept valid floors");
}

/// When the same creator creates two pools, both pool's registry entries
/// should be independently stored (keyed by pool_id, not creator address).
#[test]
fn test_m_new_5_multi_pool_creator_no_registry_collision() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &creation_fee_funds());

    // Create first pool
    let create_msg_1 = ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked("WILL_BE_CREATED_BY_FACTORY"),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "TokenA".to_string(),
            symbol: "TOKA".to_string(),
            decimal: 6,
        },
    };

    execute(deps.as_mut(), env.clone(), admin_info.clone(), create_msg_1).unwrap();
    let pool_id_1 = POOL_COUNTER.load(&deps.storage).unwrap();

    // Complete the reply chain for pool 1
    let token_1 = make_addr("token_addr_1");
    let token_reply =
        create_instantiate_reply(encode_reply_id(pool_id_1, SET_TOKENS), token_1.as_str());
    pool_creation_reply(deps.as_mut(), env.clone(), token_reply).unwrap();
    let nft_1 = make_addr("nft_addr_1");
    let nft_reply =
        create_instantiate_reply(encode_reply_id(pool_id_1, MINT_CREATE_POOL), nft_1.as_str());
    pool_creation_reply(deps.as_mut(), env.clone(), nft_reply).unwrap();
    let pool_1 = make_addr("pool_addr_1");
    let pool_reply =
        create_instantiate_reply(encode_reply_id(pool_id_1, FINALIZE_POOL), pool_1.as_str());
    pool_creation_reply(deps.as_mut(), env.clone(), pool_reply).unwrap();

    // Verify pool 1 registry info
    let pool_1_details = POOLS_BY_ID.load(&deps.storage, pool_id_1).unwrap();
    assert_eq!(pool_1_details.creator_pool_addr, pool_1.clone());
    assert_eq!(pool_1_details.pool_id, pool_id_1);

    // Create second pool from the SAME creator (admin)
    let create_msg_2 = ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked("WILL_BE_CREATED_BY_FACTORY"),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "TokenB".to_string(),
            symbol: "TOKB".to_string(),
            decimal: 6,
        },
    };

    // Per-address rate limit (1h between creates from the same
    // address). Advance the clock past the cooldown so this test
    // exercises the registry-collision path rather than the rate-limit
    // guard (which has its own dedicated tests).
    let mut env_after_cooldown = env.clone();
    env_after_cooldown.block.time = env_after_cooldown
        .block
        .time
        .plus_seconds(crate::state::COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS + 1);

    execute(
        deps.as_mut(),
        env_after_cooldown.clone(),
        admin_info,
        create_msg_2,
    )
    .unwrap();
    let pool_id_2 = POOL_COUNTER.load(&deps.storage).unwrap();
    assert_ne!(pool_id_1, pool_id_2, "Second pool should get a new ID");

    // Complete the reply chain for pool 2
    let token_2 = make_addr("token_addr_2");
    let token_reply =
        create_instantiate_reply(encode_reply_id(pool_id_2, SET_TOKENS), token_2.as_str());
    pool_creation_reply(deps.as_mut(), env_after_cooldown.clone(), token_reply).unwrap();
    let nft_2 = make_addr("nft_addr_2");
    let nft_reply =
        create_instantiate_reply(encode_reply_id(pool_id_2, MINT_CREATE_POOL), nft_2.as_str());
    pool_creation_reply(deps.as_mut(), env_after_cooldown.clone(), nft_reply).unwrap();
    let pool_2 = make_addr("pool_addr_2");
    let pool_reply =
        create_instantiate_reply(encode_reply_id(pool_id_2, FINALIZE_POOL), pool_2.as_str());
    pool_creation_reply(deps.as_mut(), env_after_cooldown, pool_reply).unwrap();

    // Verify pool 2 registry info
    let pool_2_details = POOLS_BY_ID.load(&deps.storage, pool_id_2).unwrap();
    assert_eq!(pool_2_details.creator_pool_addr, pool_2.clone());
    assert_eq!(pool_2_details.pool_id, pool_id_2);

    // KEY ASSERTION: Pool 1's registry entry should still be intact.
    // (Keying the registry by creator address would fail here, as pool 2
    // would overwrite pool 1; the pool_id key keeps both entries.)
    let pool_1_details_after = POOLS_BY_ID.load(&deps.storage, pool_id_1).unwrap();
    assert_eq!(
        pool_1_details_after.pool_id, pool_id_1,
        "Pool 1 registry entry should not be overwritten by pool 2"
    );
    assert_eq!(
        pool_1_details_after.creator_pool_addr, pool_1,
        "Pool 1 pool address should still be pool_addr_1, not pool_addr_2"
    );
}

#[test]
fn test_l_new_8_factory_migration_contract_name() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // After instantiate, cw2 should be set
    let version_info = cw2::get_contract_version(&deps.storage).unwrap();
    assert_eq!(
        version_info.contract, "crates.io:bluechip-factory",
        "Instantiate should set contract name to crates.io:bluechip-factory"
    );

    // Simulate migration (set version to older to allow migration)
    cw2::set_contract_version(&mut deps.storage, "crates.io:bluechip-factory", "0.1.0").unwrap();

    let env = mock_env();
    let res = crate::migrate::migrate(deps.as_mut(), env, Empty {});
    assert!(res.is_ok(), "Migration should succeed: {:?}", res.err());

    // After migration, contract name should still be "crates.io:bluechip-factory"
    let version_info = cw2::get_contract_version(&deps.storage).unwrap();
    assert_eq!(
        version_info.contract, "crates.io:bluechip-factory",
        "Migration should maintain the same contract name"
    );
}

// ---------------------------------------------------------------------------
// `ProposeConfigUpdate` must refuse to silently overwrite a pending
// proposal. Without this, a benign proposal at hour 47 of the timelock
// could be replaced by a hostile one minutes before the community window
// elapses, with no on-chain `Cancel` event signalling the swap.
// ---------------------------------------------------------------------------
#[test]
fn test_propose_config_update_rejects_overwrite() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let info = message_info(&admin_addr(), &[]);
    let env = mock_env();

    // First proposal — succeeds.
    execute(
        deps.as_mut(),
        env.clone(),
        info.clone(),
        ExecuteMsg::ProposeConfigUpdate {
            config: default_factory_config(),
        },
    )
    .unwrap();

    // Second proposal — must fail because PENDING_CONFIG already exists.
    let res = execute(
        deps.as_mut(),
        env,
        info,
        ExecuteMsg::ProposeConfigUpdate {
            config: default_factory_config(),
        },
    );
    let err = res.expect_err("second propose without cancel should fail");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("already pending") || err_msg.contains("CancelConfigUpdate"),
        "expected already-pending rejection, got: {}",
        err_msg
    );
}

// ---------------------------------------------------------------------------
// `validate_factory_config` must reject configs whose
// `commit_fee_bluechip + commit_fee_creator > 100%`. The pool-side
// `instantiate` already rejects with `InvalidFee`, but if the factory
// stored a bad config it would brick every subsequent `Create` until
// another 48h cycle to fix. Validating at propose-time surfaces the
// misconfig immediately.
// ---------------------------------------------------------------------------
#[test]
fn test_propose_config_update_rejects_fee_sum_above_one() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let mut bad = default_factory_config();
    bad.commit_fee_bluechip = Decimal::percent(60);
    bad.commit_fee_creator = Decimal::percent(50); // sum 110% > 1.0

    let info = message_info(&admin_addr(), &[]);
    let res = execute(
        deps.as_mut(),
        mock_env(),
        info,
        ExecuteMsg::ProposeConfigUpdate { config: bad },
    );
    let err = res.expect_err("fee sum above 1.0 must be rejected at propose time");
    assert!(err.to_string().contains("commit_fee"), "got: {}", err);
}

// ---------------------------------------------------------------------------
// `commit_threshold_limit_usd == 0` is also rejected. A zero threshold
// makes commit pools created against this config permanently uncrossable,
// locking them in pre-threshold state forever.
// ---------------------------------------------------------------------------
#[test]
fn test_propose_config_update_rejects_zero_threshold() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let mut bad = default_factory_config();
    bad.commit_threshold_limit_usd = Uint128::zero();

    let info = message_info(&admin_addr(), &[]);
    let res = execute(
        deps.as_mut(),
        mock_env(),
        info,
        ExecuteMsg::ProposeConfigUpdate { config: bad },
    );
    let err = res.expect_err("zero threshold must be rejected at propose time");
    assert!(
        err.to_string().contains("commit_threshold_limit_usd"),
        "got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Commit-pool create must REJECT non-bluechip funds and multi-denom
// payloads` + refund-extras pattern). On reject
// the tx reverts and the bank module auto-returns all attached funds to
// the caller, so orphaning is impossible regardless of denom shape.
//
// Targets the commit-pool path because it instantiates the CW20 itself
// (no external `TokenInfo` query), so the test reaches the funds-check
// deterministically without needing a CW20 mock.
// ---------------------------------------------------------------------------
#[test]
fn test_create_commit_pool_rejects_non_bluechip_funds() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let caller = make_addr("commit_pool_creator");
    let make_create_msg = || ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked(
                        crate::execute::pool_lifecycle::create::CREATOR_TOKEN_SENTINEL,
                    ),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "TestToken".to_string(),
            symbol: "TEST".to_string(),
            decimal: 6,
        },
    };

    // Case 1: bluechip + an extra denom mixed in. `must_pay` rejects
    // the multi-denom shape; the tx reverts and all attached funds
    // (both bluechip and the extra) are returned by the bank module.
    let multi_denom_funds = vec![
        Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(120_000_000),
        },
        Coin {
            denom: "ibc/27394FB...ATOM".to_string(),
            amount: Uint128::new(42_000_000),
        },
    ];
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&caller, &multi_denom_funds),
        make_create_msg(),
    )
    .expect_err("multi-denom funds must be rejected");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("Invalid commit-pool creation funds")
            || err_msg.contains("exactly one denom"),
        "error should reference must_pay-style rejection, got: {}",
        err_msg
    );

    // Case 2: only a non-bluechip denom. `must_pay` returns
    // MissingDenom; we map that to the insufficient-fee error.
    let wrong_denom_only = vec![Coin {
        denom: "ibc/27394FB...ATOM".to_string(),
        amount: Uint128::new(42_000_000),
    }];
    let caller2 = make_addr("commit_pool_creator_2");
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&caller2, &wrong_denom_only),
        make_create_msg(),
    )
    .expect_err("non-bluechip-only funds must be rejected");
    assert!(
        err.to_string().contains("Insufficient"),
        "wrong-denom-only should map to insufficient-fee error, got: {}",
        err
    );

    // Case 3: bluechip below the required fee. Must_pay returns the
    // amount; the subsequent < required check fires.
    let underpaid = vec![Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1),
    }];
    let caller3 = make_addr("commit_pool_creator_3");
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&caller3, &underpaid),
        make_create_msg(),
    )
    .expect_err("underpayment must be rejected");
    assert!(
        err.to_string().contains("Insufficient"),
        "underpayment should yield insufficient-fee error, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// When the creation fee is disabled (zero), attaching ANY funds errors —
// a disabled fee must not quietly accept-then-refund payments.
// ---------------------------------------------------------------------------
#[test]
fn test_create_commit_pool_disabled_fee_rejects_attached_funds() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    // Disable the flat fee.
    let mut cfg = default_factory_config();
    cfg.pool_creation_fee = Uint128::zero();
    crate::state::FACTORYINSTANTIATEINFO
        .save(&mut deps.storage, &cfg)
        .unwrap();

    let caller = make_addr("fee_disabled_creator");
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&caller, &creation_fee_funds()),
        ExecuteMsg::Create {
            pool_msg: CreatePool {
                pool_token_info: [
                    TokenType::Native {
                        denom: "ubluechip".to_string(),
                    },
                    TokenType::CreatorToken {
                        contract_addr: Addr::unchecked(
                            crate::execute::pool_lifecycle::create::CREATOR_TOKEN_SENTINEL,
                        ),
                    },
                ],
            },
            token_info: CreatorTokenInfo {
                name: "TestToken".to_string(),
                symbol: "TEST".to_string(),
                decimal: 6,
            },
        },
    )
    .expect_err("attaching funds while fee is disabled must be rejected");
    assert!(
        err.to_string().contains("fee is disabled"),
        "expected disabled-fee rejection, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// `ProposePoolUpgrade` must dedup `pool_ids` and reject IDs that don't
// exist in the registry. If the admin-supplied list flowed straight
// through to apply, duplicates would produce two `Migrate` messages to
// the same pool and invalid IDs would abort the entire batch after a 48h
// timelock.
// ---------------------------------------------------------------------------
#[test]
fn test_m1_propose_upgrade_rejects_unregistered_pool_id() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_1"));

    let res = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&admin_addr(), &[]),
        ExecuteMsg::UpgradePools {
            new_code_id: 99,
            // pool 1 exists; pool 999 does not.
            pool_ids: Some(vec![1, 999]),
            migrate_msg: cosmwasm_std::to_json_binary(&Empty {}).unwrap(),
        },
    );
    let err = res.expect_err("propose with unregistered id must fail");
    assert!(
        err.to_string().contains("999") && err.to_string().contains("not found"),
        "expected 'pool 999 not found' error, got: {}",
        err
    );
}

#[test]
fn test_m1_propose_upgrade_dedups_pool_ids() {
    use crate::state::PENDING_POOL_UPGRADE;
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    register_test_pool_addr(&mut deps.storage, 1, &Addr::unchecked("pool_1"));
    register_test_pool_addr(&mut deps.storage, 2, &Addr::unchecked("pool_2"));

    execute(
        deps.as_mut(),
        mock_env(),
        message_info(&admin_addr(), &[]),
        ExecuteMsg::UpgradePools {
            new_code_id: 99,
            // Duplicates of 1, plus a single 2. Expected: [1, 2] (order
            // preserved, duplicates dropped).
            pool_ids: Some(vec![1, 1, 2, 1]),
            migrate_msg: cosmwasm_std::to_json_binary(&Empty {}).unwrap(),
        },
    )
    .unwrap();

    let pending = PENDING_POOL_UPGRADE.load(&deps.storage).unwrap();
    assert_eq!(
        pending.pools_to_upgrade,
        vec![1u64, 2],
        "duplicates must be dropped, order preserved"
    );
}

// ---------------------------------------------------------------------------
// The CW20 address minted by the factory (via the SET_TOKENS reply)
// must be persisted into POOLS_BY_ID — leaving every commit pool's
// registry entry with the placeholder string would break downstream
// consumers. This test pins the invariant: registry's
// CreatorToken address matches the SubMsg-instantiated CW20.
// ---------------------------------------------------------------------------
#[test]
fn test_c2_pool_details_persists_real_creator_token_address() {
    use crate::execute::pool_lifecycle::create::CREATOR_TOKEN_SENTINEL;

    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let env = mock_env();
    let admin_info = message_info(&admin_addr(), &creation_fee_funds());

    // Caller-supplied pair carries the SENTINEL — the factory mints the
    // CW20 itself and rewrites the address downstream.
    let create_msg = ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked(CREATOR_TOKEN_SENTINEL),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "TestToken".to_string(),
            symbol: "TEST".to_string(),
            decimal: 6,
        },
    };

    execute(deps.as_mut(), env.clone(), admin_info, create_msg).unwrap();
    let pool_id = POOL_COUNTER.load(&deps.storage).unwrap();

    // Walk the reply chain. The address fed into SET_TOKENS is the one
    // we expect to find in POOLS_BY_ID at the end.
    let real_token_addr = make_addr("freshly_instantiated_cw20");
    let token_reply = create_instantiate_reply(
        encode_reply_id(pool_id, SET_TOKENS),
        real_token_addr.as_str(),
    );
    pool_creation_reply(deps.as_mut(), env.clone(), token_reply).unwrap();
    let nft_addr = make_addr("freshly_instantiated_cw721");
    let nft_reply = create_instantiate_reply(
        encode_reply_id(pool_id, MINT_CREATE_POOL),
        nft_addr.as_str(),
    );
    pool_creation_reply(deps.as_mut(), env.clone(), nft_reply).unwrap();
    let pool_addr = make_addr("freshly_instantiated_pool");
    let pool_reply =
        create_instantiate_reply(encode_reply_id(pool_id, FINALIZE_POOL), pool_addr.as_str());
    pool_creation_reply(deps.as_mut(), env, pool_reply).unwrap();

    // Invariant: PoolDetails.pool_token_info[1] must be the REAL CW20,
    // not the sentinel placeholder.
    let details = POOLS_BY_ID.load(&deps.storage, pool_id).unwrap();
    let creator_token_addr = match &details.pool_token_info[1] {
        TokenType::CreatorToken { contract_addr } => contract_addr.clone(),
        _ => panic!(
            "expected CreatorToken at pool_token_info[1], got: {:?}",
            details.pool_token_info[1]
        ),
    };
    assert_ne!(
        creator_token_addr.as_str(),
        CREATOR_TOKEN_SENTINEL,
        "regression: PoolDetails persisted the sentinel instead of the real CW20 address"
    );
    assert_eq!(
        creator_token_addr, real_token_addr,
        "PoolDetails CreatorToken address must equal the SubMsg-instantiated CW20"
    );

    // The asset_strings stored in POOLS_BY_CONTRACT_ADDRESS (used by
    // off-chain query consumers) is derived from pool_token_info — it
    // must also have the real address, not the sentinel.
    let snapshot = POOLS_BY_CONTRACT_ADDRESS
        .load(&deps.storage, pool_addr)
        .unwrap();
    assert!(
        snapshot
            .assets
            .iter()
            .any(|a| a == real_token_addr.as_str()),
        "POOLS_BY_CONTRACT_ADDRESS.assets must include the real CW20 address; got: {:?}",
        snapshot.assets
    );
    assert!(
        !snapshot.assets.iter().any(|a| a == CREATOR_TOKEN_SENTINEL),
        "POOLS_BY_CONTRACT_ADDRESS.assets must not retain the sentinel; got: {:?}",
        snapshot.assets
    );
}

// ---------------------------------------------------------------------------
// `validate_creator_token_info` rejects all-numeric symbols.
// ---------------------------------------------------------------------------
#[test]
fn test_l7_create_rejects_all_numeric_symbol() {
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let res = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&admin_addr(), &[]),
        ExecuteMsg::Create {
            pool_msg: CreatePool {
                pool_token_info: [
                    TokenType::Native {
                        denom: "ubluechip".to_string(),
                    },
                    TokenType::CreatorToken {
                        contract_addr: Addr::unchecked(
                            crate::execute::pool_lifecycle::create::CREATOR_TOKEN_SENTINEL,
                        ),
                    },
                ],
            },
            token_info: CreatorTokenInfo {
                name: "All-digit symbol token".to_string(),
                symbol: "12345".to_string(),
                decimal: 6,
            },
        },
    );
    let err = res.expect_err("all-numeric symbol must be rejected");
    assert!(
        err.to_string()
            .contains("at least one uppercase ASCII letter"),
        "expected letter-required error, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Per-address rate limit on commit-pool creation. Without it, anyone
// could spam consecutive Create calls. The same address must wait
// `COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS` between successful creates.
// ---------------------------------------------------------------------------
#[test]
fn test_i6_commit_pool_create_rate_limit_per_address() {
    use crate::execute::pool_lifecycle::create::CREATOR_TOKEN_SENTINEL;
    let mut deps = mock_deps_with_querier(&[]);
    setup_factory(&mut deps);

    let make_msg = |sym: &str| ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked(CREATOR_TOKEN_SENTINEL),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: format!("Token {}", sym),
            symbol: sym.to_string(),
            decimal: 6,
        },
    };

    let env = mock_env();
    let info = message_info(&admin_addr(), &creation_fee_funds());

    // First create: succeeds.
    execute(deps.as_mut(), env.clone(), info.clone(), make_msg("AAA")).unwrap();

    // Second create from the same address, same block: rate-limited.
    let res = execute(deps.as_mut(), env.clone(), info.clone(), make_msg("BBB"));
    let err = res.expect_err("rapid second create from same address must be rate-limited");
    assert!(
        err.to_string().contains("Rate-limited"),
        "expected rate-limit error, got: {}",
        err
    );

    // From a DIFFERENT address in the same block: allowed (per-address gate).
    let other = make_addr("other_creator");
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&other, &creation_fee_funds()),
        make_msg("CCC"),
    )
    .unwrap();

    // After the cooldown elapses, the original address can create again.
    let mut later_env = env.clone();
    later_env.block.time = later_env
        .block
        .time
        .plus_seconds(crate::state::COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS + 1);
    execute(deps.as_mut(), later_env, info, make_msg("DDD")).unwrap();
}

// ---------------------------------------------------------------------------
// Factory → pool admin forwarders (pause / unpause / emergency withdraw /
// stuck-state recovery). The factory validates admin + pool registration,
// then forwards a `PoolAdminMsg` to the pool contract.
// ---------------------------------------------------------------------------
mod pool_admin_forwarder_tests {
    use super::*;
    use cosmwasm_std::{from_json, CosmosMsg, WasmMsg};
    use serde::{Deserialize, Serialize};

    /// Mirror of the factory's private `PoolAdminMsg` enum so tests can
    /// decode and assert on the forwarded body. Wire format must stay in
    /// lock-step with `factory/src/execute/pool_lifecycle/admin.rs`.
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum PoolAdminMsgMirror {
        Pause {},
        Unpause {},
        EmergencyWithdraw {},
        CancelEmergencyWithdraw {},
        RecoverStuckStates {
            recovery_type: crate::pool_struct::RecoveryType,
        },
    }

    fn setup_factory_with_pool(
        pool_id: u64,
    ) -> (OwnedDeps<MockStorage, MockApi, WasmMockQuerier>, Addr) {
        let mut deps = mock_deps_with_querier(&[]);
        setup_factory(&mut deps);
        let pool_addr = make_addr(&format!("pool_{}", pool_id));
        register_test_pool_addr(&mut deps.storage, pool_id, &pool_addr);
        (deps, pool_addr)
    }

    fn assert_forwards_to_pool(
        res: cosmwasm_std::Response,
        expected_pool_addr: &Addr,
        expected_inner: PoolAdminMsgMirror,
    ) {
        assert_eq!(res.messages.len(), 1, "expected exactly one forwarded msg");
        match &res.messages[0].msg {
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr,
                msg,
                funds,
            }) => {
                assert_eq!(contract_addr, &expected_pool_addr.to_string());
                assert!(funds.is_empty(), "admin forwards must not attach funds");
                let inner: PoolAdminMsgMirror = from_json(msg).unwrap();
                assert_eq!(inner, expected_inner);
            }
            other => panic!("expected WasmMsg::Execute, got {:?}", other),
        }
    }

    #[test]
    fn pause_pool_admin_forwards_to_pool() {
        let (mut deps, pool_addr) = setup_factory_with_pool(42);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&admin_addr(), &[]),
            ExecuteMsg::PausePool { pool_id: 42 },
        )
        .unwrap();
        assert_forwards_to_pool(res, &pool_addr, PoolAdminMsgMirror::Pause {});
    }

    #[test]
    fn pause_pool_non_admin_rejected() {
        let (mut deps, _) = setup_factory_with_pool(42);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked("hacker"), &[]),
            ExecuteMsg::PausePool { pool_id: 42 },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::Unauthorized {}));
    }

    #[test]
    fn pause_pool_unknown_pool_id_rejected() {
        let (mut deps, _) = setup_factory_with_pool(42);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&admin_addr(), &[]),
            ExecuteMsg::PausePool { pool_id: 999 }, // not registered
        )
        .unwrap_err();
        assert!(err.to_string().contains("not found in registry"));
    }

    #[test]
    fn unpause_pool_admin_forwards_to_pool() {
        let (mut deps, pool_addr) = setup_factory_with_pool(7);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&admin_addr(), &[]),
            ExecuteMsg::UnpausePool { pool_id: 7 },
        )
        .unwrap();
        assert_forwards_to_pool(res, &pool_addr, PoolAdminMsgMirror::Unpause {});
    }

    #[test]
    fn unpause_pool_non_admin_rejected() {
        let (mut deps, _) = setup_factory_with_pool(7);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked("hacker"), &[]),
            ExecuteMsg::UnpausePool { pool_id: 7 },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::Unauthorized {}));
    }

    #[test]
    fn emergency_withdraw_admin_forwards_to_pool() {
        let (mut deps, pool_addr) = setup_factory_with_pool(123);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&admin_addr(), &[]),
            ExecuteMsg::EmergencyWithdrawPool { pool_id: 123 },
        )
        .unwrap();
        assert_forwards_to_pool(res, &pool_addr, PoolAdminMsgMirror::EmergencyWithdraw {});
    }

    #[test]
    fn emergency_withdraw_non_admin_rejected() {
        let (mut deps, _) = setup_factory_with_pool(123);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked("hacker"), &[]),
            ExecuteMsg::EmergencyWithdrawPool { pool_id: 123 },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::Unauthorized {}));
    }

    #[test]
    fn cancel_emergency_withdraw_admin_forwards_to_pool() {
        let (mut deps, pool_addr) = setup_factory_with_pool(456);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&admin_addr(), &[]),
            ExecuteMsg::CancelEmergencyWithdrawPool { pool_id: 456 },
        )
        .unwrap();
        assert_forwards_to_pool(
            res,
            &pool_addr,
            PoolAdminMsgMirror::CancelEmergencyWithdraw {},
        );
    }

    #[test]
    fn recover_stuck_states_admin_forwards_to_pool_with_recovery_type() {
        let (mut deps, pool_addr) = setup_factory_with_pool(99);
        // Each RecoveryType variant must round-trip through the forwarded
        // payload — exercise all four.
        for recovery in [
            crate::pool_struct::RecoveryType::StuckThreshold,
            crate::pool_struct::RecoveryType::StuckDistribution,
            crate::pool_struct::RecoveryType::StuckReentrancyGuard,
            crate::pool_struct::RecoveryType::Both,
        ] {
            let res = execute(
                deps.as_mut(),
                mock_env(),
                message_info(&admin_addr(), &[]),
                ExecuteMsg::RecoverPoolStuckStates {
                    pool_id: 99,
                    recovery_type: recovery.clone(),
                },
            )
            .unwrap();
            assert_forwards_to_pool(
                res,
                &pool_addr,
                PoolAdminMsgMirror::RecoverStuckStates {
                    recovery_type: recovery,
                },
            );
        }
    }

    #[test]
    fn recover_stuck_states_non_admin_rejected() {
        let (mut deps, _) = setup_factory_with_pool(99);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            message_info(&Addr::unchecked("hacker"), &[]),
            ExecuteMsg::RecoverPoolStuckStates {
                pool_id: 99,
                recovery_type: crate::pool_struct::RecoveryType::StuckThreshold,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::Unauthorized {}));
    }
}

// ---------------------------------------------------------------------------
// Pair-uniqueness guard: canonical pair key, register_pool duplicate
// rejection, and the migrate back-fill of PAIRS from POOLS_BY_ID.
// ---------------------------------------------------------------------------
mod pair_uniqueness_tests {
    use super::*;
    use crate::state::{canonical_pair_key, register_pool, PAIRS};

    fn pool_details_for(pair: [TokenType; 2], pool_id: u64) -> PoolDetails {
        PoolDetails {
            pool_id,
            pool_token_info: pair,
            creator_pool_addr: make_addr(&format!("pool_{}", pool_id)),
        }
    }

    /// canonical_pair_key([A, B]) must equal canonical_pair_key([B, A]).
    /// Without this property, registering [A, B] then [B, A] would land
    /// on different storage slots and the uniqueness guard would silently
    /// admit duplicates that differed only in argument order.
    #[test]
    fn canonical_pair_key_is_order_independent() {
        let a = TokenType::Native {
            denom: "ubluechip".to_string(),
        };
        let b = TokenType::Native {
            denom: "uatom".to_string(),
        };
        assert_eq!(
            canonical_pair_key(&[a.clone(), b.clone()]),
            canonical_pair_key(&[b, a]),
        );
    }

    /// A native denom that happens to look like a CW20 contract-address
    /// string must NOT collide with a CreatorToken whose `contract_addr`
    /// equals that string. The `n:` / `c:` prefixes keep the namespaces
    /// disjoint.
    #[test]
    fn canonical_pair_key_separates_native_and_cw20_namespaces() {
        let collision_string = "cosmos1abc";
        let native_pair = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::Native {
                denom: collision_string.to_string(),
            },
        ];
        let cw20_pair = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::CreatorToken {
                contract_addr: Addr::unchecked(collision_string),
            },
        ];
        assert_ne!(
            canonical_pair_key(&native_pair),
            canonical_pair_key(&cw20_pair)
        );
    }

    /// register_pool: first call records the pair in PAIRS; second call
    /// with the same pair (different pool_id, different address) errors
    /// with the canonical "duplicate pair" message.
    #[test]
    fn register_pool_rejects_duplicate_pair_at_canonical_guard() {
        let mut deps = mock_deps_with_querier(&[]);
        setup_factory(&mut deps);

        let pair = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::Native {
                denom: "uatom".to_string(),
            },
        ];

        let pool1 = pool_details_for(pair.clone(), 1);
        register_pool(
            deps.as_mut().storage,
            1,
            &pool1.creator_pool_addr.clone(),
            &pool1,
        )
        .expect("first registration must succeed");

        // PAIRS now contains the canonical key pointing at pool 1.
        let stored = PAIRS
            .may_load(&deps.storage, canonical_pair_key(&pair))
            .unwrap();
        assert_eq!(stored, Some(1));

        // Second registration with the same pair must fail. We use a
        // different pool_id and a different contract address to make
        // sure the rejection is keyed on the pair, not on either of
        // those.
        let pool2 = pool_details_for(pair.clone(), 2);
        let err = register_pool(
            deps.as_mut().storage,
            2,
            &pool2.creator_pool_addr.clone(),
            &pool2,
        )
        .expect_err("duplicate pair must be rejected at register_pool");
        assert!(
            err.to_string().contains("duplicate pair"),
            "expected duplicate-pair error, got: {}",
            err
        );

        // Reverse-order pair must also be rejected (canonicalization).
        let reversed = [pair[1].clone(), pair[0].clone()];
        let pool3 = pool_details_for(reversed, 3);
        let err = register_pool(
            deps.as_mut().storage,
            3,
            &pool3.creator_pool_addr.clone(),
            &pool3,
        )
        .expect_err("reversed-order duplicate must be rejected");
        assert!(
            err.to_string().contains("duplicate pair"),
            "expected duplicate-pair error on reversed order, got: {}",
            err
        );
    }

    /// Migrate back-fill: after instantiating the factory and seeding
    /// `POOLS_BY_ID` directly with two distinct pools (different pairs),
    /// migrate must populate `PAIRS` with one entry per pair.
    #[test]
    fn migrate_backfills_pairs_from_existing_pools() {
        let mut deps = mock_deps_with_querier(&[]);
        setup_factory(&mut deps);

        let pair1 = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::Native {
                denom: "uatom".to_string(),
            },
        ];
        let pair2 = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::CreatorToken {
                contract_addr: make_addr("creator_token_xyz"),
            },
        ];

        POOLS_BY_ID
            .save(
                deps.as_mut().storage,
                10,
                &pool_details_for(pair1.clone(), 10),
            )
            .unwrap();
        POOLS_BY_ID
            .save(
                deps.as_mut().storage,
                11,
                &pool_details_for(pair2.clone(), 11),
            )
            .unwrap();

        // PAIRS empty pre-migrate.
        assert!(PAIRS
            .may_load(&deps.storage, canonical_pair_key(&pair1))
            .unwrap()
            .is_none());

        cw2::set_contract_version(&mut deps.storage, "crates.io:bluechip-factory", "0.1.0")
            .unwrap();
        let res = crate::migrate::migrate(deps.as_mut(), mock_env(), Empty {}).expect("migrate ok");

        assert_eq!(
            PAIRS
                .may_load(&deps.storage, canonical_pair_key(&pair1))
                .unwrap(),
            Some(10),
        );
        assert_eq!(
            PAIRS
                .may_load(&deps.storage, canonical_pair_key(&pair2))
                .unwrap(),
            Some(11),
        );
        // Observability: backfilled count surfaced as an attribute.
        let backfilled = res
            .attributes
            .iter()
            .find(|a| a.key == "pairs_backfilled")
            .map(|a| a.value.as_str());
        assert_eq!(backfilled, Some("2"));
        let legacy = res
            .attributes
            .iter()
            .find(|a| a.key == "legacy_duplicate_pairs_skipped")
            .map(|a| a.value.as_str());
        assert_eq!(legacy, Some("0"));
    }

    /// Migrate must preserve legacy duplicates (FIRST pool_id wins) and
    /// surface the skip count as an observability attribute. Lower
    /// pool_id wins because `POOLS_BY_ID.range(..)` iterates ascending.
    #[test]
    fn migrate_preserves_legacy_duplicates_first_pool_wins() {
        let mut deps = mock_deps_with_querier(&[]);
        setup_factory(&mut deps);

        let pair = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::Native {
                denom: "uatom".to_string(),
            },
        ];

        // Two legacy duplicate pools at the same pair — the sybil-attack
        // shape that deployed chain state may already contain and that
        // the back-fill must grandfather rather than clobber.
        POOLS_BY_ID
            .save(deps.as_mut().storage, 5, &pool_details_for(pair.clone(), 5))
            .unwrap();
        POOLS_BY_ID
            .save(deps.as_mut().storage, 9, &pool_details_for(pair.clone(), 9))
            .unwrap();

        cw2::set_contract_version(&mut deps.storage, "crates.io:bluechip-factory", "0.1.0")
            .unwrap();
        let res = crate::migrate::migrate(deps.as_mut(), mock_env(), Empty {}).expect("migrate ok");

        // First-seen (lowest pool_id) wins.
        assert_eq!(
            PAIRS
                .may_load(&deps.storage, canonical_pair_key(&pair))
                .unwrap(),
            Some(5),
        );
        let legacy = res
            .attributes
            .iter()
            .find(|a| a.key == "legacy_duplicate_pairs_skipped")
            .map(|a| a.value.as_str());
        assert_eq!(legacy, Some("1"));

        // Crucially: post-migrate, NEW duplicate registrations of this
        // pair must reject. This is what the back-fill actually buys us —
        // legacy chain state is grandfathered, but the invariant kicks
        // in for every subsequent registration.
        let new_pool = pool_details_for(pair, 99);
        let err = register_pool(
            deps.as_mut().storage,
            99,
            &new_pool.creator_pool_addr.clone(),
            &new_pool,
        )
        .expect_err("post-migrate duplicate must reject");
        assert!(
            err.to_string().contains("duplicate pair"),
            "post-migrate: expected duplicate-pair rejection pointing at the grandfathered pair; got: {}",
            err
        );
    }

    /// Migrate is idempotent: running it twice must not duplicate-insert
    /// or change the recorded pool_id (no-op on second run).
    #[test]
    fn migrate_pairs_backfill_is_idempotent() {
        let mut deps = mock_deps_with_querier(&[]);
        setup_factory(&mut deps);

        let pair = [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::Native {
                denom: "uatom".to_string(),
            },
        ];
        POOLS_BY_ID
            .save(
                deps.as_mut().storage,
                42,
                &pool_details_for(pair.clone(), 42),
            )
            .unwrap();

        cw2::set_contract_version(&mut deps.storage, "crates.io:bluechip-factory", "0.1.0")
            .unwrap();
        crate::migrate::migrate(deps.as_mut(), mock_env(), Empty {}).expect("first migrate ok");
        // Second migrate: stored version was just written to CONTRACT_VERSION
        // (current). Reset to an older value so the migrate handler accepts
        // the re-run rather than no-op-ing through the equal-version branch.
        cw2::set_contract_version(&mut deps.storage, "crates.io:bluechip-factory", "0.1.0")
            .unwrap();
        let res2 =
            crate::migrate::migrate(deps.as_mut(), mock_env(), Empty {}).expect("re-migrate ok");

        assert_eq!(
            PAIRS
                .may_load(&deps.storage, canonical_pair_key(&pair))
                .unwrap(),
            Some(42),
            "pool_id must NOT change on re-run",
        );
        // Re-run sees the entry already populated → backfilled=0.
        let backfilled = res2
            .attributes
            .iter()
            .find(|a| a.key == "pairs_backfilled")
            .map(|a| a.value.as_str());
        assert_eq!(backfilled, Some("0"));
    }
}
