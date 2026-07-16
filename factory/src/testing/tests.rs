use crate::state::{FactoryInstantiate, POOLS_BY_ID, POOL_COUNTER};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Binary, Coin, Decimal, Env, Event, OwnedDeps, Reply,
    SubMsgResponse, SubMsgResult, Uint128,
};

use crate::asset::{TokenInfo, TokenType};
use crate::execute::{
    encode_reply_id, execute, instantiate, pool_creation_reply, FINALIZE_POOL,
};
use crate::mock_querier::{mock_dependencies, WasmMockQuerier};
use crate::msg::{CreatorTokenInfo, ExecuteMsg};
use crate::pool_struct::{CreatePool, PoolDetails, TempPoolCreation};
use cosmwasm_std::testing::{message_info, mock_env, MockApi, MockStorage};

fn admin_addr() -> Addr {
    MockApi::default().addr_make("admin")
}

fn ubluechip_addr() -> Addr {
    MockApi::default().addr_make("ubluechip")
}

/// Funds covering the flat commit-pool creation fee in `info.funds`.
/// The default test config sets `pool_creation_fee` to
/// 1_000_000 ubluechip; paying 100_000_000 comfortably covers any fee a
/// test configures (the handler refunds the surplus in the same tx).
pub(crate) fn creation_fee_funds() -> [Coin; 1] {
    [Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(100_000_000),
    }]
}

fn addr0000() -> Addr {
    MockApi::default().addr_make("addr0000")
}

fn make_addr(label: &str) -> Addr {
    MockApi::default().addr_make(label)
}
#[cfg(test)]
fn create_default_instantiate_msg() -> FactoryInstantiate {
    FactoryInstantiate {
        factory_admin_address: admin_addr(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(1),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    }
}

/// Save a minimal `PoolDetails` for `pool_id` so production code that looks
/// up a pool address via `POOLS_BY_ID.load(..).creator_pool_addr` works in
/// tests. Mirrors the pre-consolidation `POOL_REGISTRY.save(..., &addr)`
/// convenience; the extra fields default to values no test cares about.
pub fn register_test_pool_addr(
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
                        denom: String::from("token"),
                    },
                ],
                creator_pool_addr: pool_addr.clone(),
            },
        )
        .unwrap();
    // Mirror `state::register_pool` — the reverse address->id index is a
    // load-bearing invariant that `lookup_pool_by_addr` depends on.
    // Bypassing it in tests would leave any handler that resolves a pool
    // by address (e.g. NotifyThresholdCrossed) unable to find the fixture.
    crate::state::POOL_ID_BY_ADDRESS
        .save(storage, pool_addr.clone(), &pool_id)
        .unwrap();
}

#[test]
fn proper_initialization() {
    let mut deps = mock_dependencies(&[]);

    let the_admin = addr0000();
    let msg = FactoryInstantiate {
        factory_admin_address: the_admin.clone(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(100),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(10),
        commit_fee_creator: Decimal::percent(10),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    let env = mock_env();
    let info = message_info(&the_admin, &[]);

    let res = instantiate(deps.as_mut(), env.clone(), info, msg.clone()).unwrap();

    assert!(res
        .attributes
        .iter()
        .any(|attr| attr.key == "action" && attr.value == "init_contract"));

    let mut deps2 = mock_dependencies(&[]);

    let env = mock_env();
    let info = message_info(&the_admin, &[]);

    let _res1 = instantiate(deps2.as_mut(), env.clone(), info, msg.clone()).unwrap();

    let mut deps3 = mock_dependencies(&[]);

    let env = mock_env();
    let info = message_info(&the_admin, &[]);

    instantiate(deps3.as_mut(), env.clone(), info, msg.clone()).unwrap();
}

#[test]
fn create_pair() {
    let mut deps = mock_dependencies(&[]);

    let the_admin = addr0000();
    let msg = FactoryInstantiate {
        factory_admin_address: the_admin.clone(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    let env = mock_env();
    let info = message_info(&the_admin, &[]);

    let _res = instantiate(deps.as_mut(), env, info, msg.clone()).unwrap();

    let pool_token_info = [
        TokenType::Native {
            denom: "ubluechip".to_string(),
        },
        TokenType::CreatorToken {
            denom: String::from("WILL_BE_CREATED_BY_FACTORY"),
        },
    ];

    let env = mock_env();
    let info = message_info(&the_admin, &creation_fee_funds());

    let res = execute(
        deps.as_mut(),
        env,
        info,
        ExecuteMsg::Create {
            pool_msg: CreatePool {
                pool_token_info: pool_token_info.clone(),
            },
            token_info: CreatorTokenInfo {
                name: "Test Token".to_string(),
                symbol: "TEST".to_string(),
                decimal: 6,
            },
        },
    )
    .unwrap();

    assert!(res
        .attributes
        .iter()
        .any(|attr| attr.key == "action" && attr.value == "create"));
    assert!(res.attributes.iter().any(|attr| attr.key == "creator"));
    assert!(res.attributes.iter().any(|attr| attr.key == "pool_id"));
    // Flat native creation fee comes straight from config — no oracle.
    assert!(
        res.attributes
            .iter()
            .any(|attr| attr.key == "fee_source" && attr.value == "config"),
        "fee_source must be \"config\" when pool_creation_fee > 0"
    );
}

#[test]
fn create_pair_fee_disabled_rejects_attached_funds() {
    // With pool_creation_fee == 0 the fee gate is disabled:
    // a fund-less Create succeeds (fee_source = "disabled") and any
    // attached funds are rejected outright.
    let mut deps = mock_dependencies(&[]);

    let mut msg = create_default_instantiate_msg();
    msg.pool_creation_fee = Uint128::zero();
    let env = mock_env();
    instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin_addr(), &[]),
        msg,
    )
    .unwrap();

    // Attached funds while the fee is disabled -> hard error.
    let err = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&admin_addr(), &creation_fee_funds()),
        create_pool_msg("Token1"),
    )
    .unwrap_err();
    assert!(
        format!("{}", err).contains("fee is disabled; do not attach any funds"),
        "got: {}",
        err
    );

    // No funds -> create succeeds and reports the disabled fee source.
    // (Advance past the per-address create cooldown: the failed attempt
    // above already stamped the rate-limit timestamp, and mock storage
    // does not roll back on handler error.)
    let mut env2 = mock_env();
    env2.block.time = env2
        .block
        .time
        .plus_seconds(crate::state::COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS + 1);
    let res = execute(
        deps.as_mut(),
        env2,
        message_info(&admin_addr(), &[]),
        create_pool_msg("Token1"),
    )
    .unwrap();
    assert!(
        res.attributes
            .iter()
            .any(|attr| attr.key == "fee_source" && attr.value == "disabled"),
        "fee_source must be \"disabled\" when pool_creation_fee == 0"
    );
}

#[test]
fn test_create_pair_with_custom_params() {
    let mut deps = mock_dependencies(&[]);

    let msg = FactoryInstantiate {
        factory_admin_address: admin_addr(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    let env = mock_env();
    let info = message_info(&admin_addr(), &[]);
    instantiate(deps.as_mut(), env, info, msg).unwrap();

    // CreatePool carries only `pool_token_info` — see
    // `pool_struct::CreatePool`. The factory config is the single
    // source of truth for threshold params.

    let create_msg = ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    denom: String::from("WILL_BE_CREATED_BY_FACTORY"),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "Custom Token".to_string(),
            symbol: "CUSTOM".to_string(),
            decimal: 6,
        },
    };

    let env = mock_env();
    let info = message_info(&admin_addr(), &creation_fee_funds());
    let res = execute(deps.as_mut(), env, info, create_msg).unwrap();

    // 1-3 messages: pool instantiate submessage + optional fee BankMsg +
    // optional surplus-refund BankMsg from the creation-fee gate.
    assert!(
        !res.messages.is_empty() && res.messages.len() <= 3,
        "Should have 1-3 messages (pool instantiate + fee + optional surplus refund), got {}",
        res.messages.len()
    );
}

fn create_pool_msg(name: &str) -> ExecuteMsg {
    ExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    denom: String::from("WILL_BE_CREATED_BY_FACTORY"),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: name.to_string(),
            // Uppercase so the symbol passes factory validation (A-Z, 0-9 only).
            symbol: name.to_uppercase(),
            decimal: 6,
        },
    }
}

fn simulate_complete_reply_chain(
    deps: &mut OwnedDeps<MockStorage, MockApi, WasmMockQuerier>,
    env: Env,
    pool_id: u64,
    create_res: &cosmwasm_std::Response,
) {
    // Phase-2 single-step reply chain: the create handler dispatched the
    // pool instantiate directly (reply id FINALIZE_POOL); its reply
    // finalizes/registers the pool. There is no NFT or CW20 instantiate step.
    let pool_addr = make_addr(&format!("pool_address_{}", pool_id));
    let pool_reply = create_instantiate_reply(
        encode_reply_id(pool_id, FINALIZE_POOL),
        pool_addr.as_str(),
        creation_payload(create_res),
    );
    pool_creation_reply(deps.as_mut(), env.clone(), pool_reply).unwrap();
}

#[test]
fn test_asset_info() {
    let bluechip_info = TokenType::Native {
        denom: "ubluechip".to_string(),
    };
    assert!(bluechip_info.is_native_token());

    let token_info = TokenType::CreatorToken {
        denom: String::from("bluechip..."),
    };
    // Post-migration: the creator token is a native TokenFactory denom, so
    // `is_native_token()` now returns true for BOTH variants.
    assert!(token_info.is_native_token());

    assert!(bluechip_info.equal(&TokenType::Native {
        denom: "ubluechip".to_string(),
    }));
    assert!(!bluechip_info.equal(&token_info));
}

/// The creation context rides SubMsg payloads through the reply chain
/// (wasmd echoes a SubMsg's `payload` back in its Reply). Tests thread
/// it the same way: extract the payload from each step's response and
/// attach it to the next simulated Reply.
pub fn creation_payload(res: &cosmwasm_std::Response) -> Binary {
    res.messages
        .iter()
        .find(|m| !m.payload.is_empty())
        .map(|m| m.payload.clone())
        .expect("response should carry a pool-creation payload on its SubMsg")
}

#[allow(deprecated)]
pub fn create_instantiate_reply(id: u64, contract_addr: &str, payload: Binary) -> Reply {
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
        payload,
    }
}

#[test]
fn test_multiple_pool_creation() {
    let mut deps = mock_dependencies(&[]);

    let msg = create_default_instantiate_msg();
    let env = mock_env();
    let info = message_info(&admin_addr(), &[]);
    instantiate(deps.as_mut(), env.clone(), info, msg).unwrap();

    // Create 3 pools and verify they're created with unique IDs
    let mut created_pool_ids = Vec::new();

    for i in 1u64..=3u64 {
        // Per-address rate limit (1h between creates from the same
        // address). Advance the clock past the cooldown for each iteration
        // so this test exercises the multi-pool registry path rather than
        // the rate-limit guard (which has its own dedicated tests).
        let mut iter_env = env.clone();
        iter_env.block.time = iter_env
            .block
            .time
            .plus_seconds((i - 1) * (crate::state::COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS + 1));

        // Create pool
        let create_msg = create_pool_msg(&format!("Token{}", i));
        let info = message_info(&admin_addr(), &creation_fee_funds());
        let res = execute(deps.as_mut(), iter_env, info, create_msg).unwrap();

        assert!(
            res.attributes.iter().any(|attr| attr.key == "pool_id"),
            "Response should contain pool_id attribute"
        );

        // The creation context rides the create response's SubMsg
        // payload (use loop index as pool_id)
        let pool_id = i;
        let temp: TempPoolCreation = from_json(creation_payload(&res)).unwrap();

        // Verify this is a new unique ID
        assert!(
            !created_pool_ids.contains(&pool_id),
            "Pool ID {} should be unique",
            pool_id
        );
        created_pool_ids.push(pool_id);

        // The payload carries the creator and the derived subdenom.
        // Subdenom = symbol.to_lowercase(); the symbol is
        // `format!("Token{i}").to_uppercase()` => "TOKEN{i}".
        assert_eq!(temp.pool_id, pool_id);
        assert_eq!(temp.temp_creator_wallet, admin_addr());
        assert_eq!(temp.subdenom, format!("token{}", i));

        // Simulate complete reply chain with the actual pool_id
        simulate_complete_reply_chain(&mut deps, env.clone(), pool_id, &res);

        assert!(
            POOLS_BY_ID.load(&deps.storage, pool_id).is_ok(),
            "Pool should be stored by ID"
        );
    }

    // Verify 3 unique pools
    assert_eq!(created_pool_ids.len(), 3, "Should have created 3 pools");
}
#[test]
fn test_complete_pool_creation_flow() {
    let mut deps = mock_dependencies(&[]);

    let msg = FactoryInstantiate {
        factory_admin_address: admin_addr(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    let env = mock_env();
    let info = message_info(&admin_addr(), &[]);
    instantiate(deps.as_mut(), env.clone(), info, msg).unwrap();

    // Create the pool message
    let pool_msg = CreatePool {
        pool_token_info: [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::CreatorToken {
                denom: String::from("WILL_BE_CREATED_BY_FACTORY"),
            },
        ],
    };

    let create_msg = ExecuteMsg::Create {
        pool_msg: pool_msg.clone(),
        token_info: CreatorTokenInfo {
            name: "Test Token".to_string(),
            symbol: "TEST".to_string(),
            decimal: 6,
        },
    };

    let info = message_info(&admin_addr(), &creation_fee_funds());
    let res = execute(deps.as_mut(), env.clone(), info, create_msg).unwrap();

    assert!(
        !res.attributes.is_empty(),
        "Should have response attributes"
    );
    // 1-3 messages: pool instantiate submessage (always) + fee BankMsg to
    // wallet (when required > 0) + optional surplus refund BankMsg when the
    // caller overpays the flat native fee. Phase-2: the factory instantiates
    // neither a CW20 nor an NFT — the pool owns its TokenFactory creator denom.
    assert!(
        !res.messages.is_empty() && res.messages.len() <= 3,
        "Should have 1-3 messages (pool instantiate + fee + optional surplus refund), got {}",
        res.messages.len()
    );

    let pool_id = POOL_COUNTER.load(&deps.storage).unwrap();
    // The creation context rides the create response's SubMsg payload.
    let temp: TempPoolCreation = from_json(creation_payload(&res)).unwrap();

    assert!(pool_id > 0);
    assert_eq!(temp.temp_creator_wallet, admin_addr());
    // subdenom = symbol.to_lowercase(); symbol "TEST" => "test".
    assert_eq!(temp.subdenom, "test");

    // Phase-2 single-step reply chain: the create handler dispatched the
    // pool instantiate directly (reply id FINALIZE_POOL); its reply
    // finalizes/registers the pool. There is no NFT or CW20 step.
    let pool_addr = make_addr("pool_address");
    let pool_reply = create_instantiate_reply(
        encode_reply_id(pool_id, FINALIZE_POOL),
        pool_addr.as_str(),
        creation_payload(&res),
    );
    let res = pool_creation_reply(deps.as_mut(), env.clone(), pool_reply).unwrap();

    let pool_by_id = POOLS_BY_ID.load(&deps.storage, pool_id).unwrap();
    assert_eq!(pool_by_id.pool_id, pool_id);
    assert_eq!(pool_by_id.creator_pool_addr, pool_addr.clone());
    // Registry persists the REAL pool-owned creator denom
    // `factory/{pool_addr}/{subdenom}`, not the caller placeholder.
    assert_eq!(
        pool_by_id.pool_token_info[1],
        TokenType::CreatorToken {
            denom: format!("factory/{}/test", pool_addr),
        }
    );

    // finalize_pool only registers the pool across the three registry maps;
    // it emits no messages (no NFT ownership handoff anymore).
    assert!(res.messages.is_empty());
    // register_pool writes all three coupled registry maps atomically.
    assert_eq!(
        crate::state::POOL_ID_BY_ADDRESS
            .load(&deps.storage, pool_addr.clone())
            .unwrap(),
        pool_id
    );
    assert!(crate::state::POOLS_BY_CONTRACT_ADDRESS.has(&deps.storage, pool_addr.clone()));
    let pair_key = crate::state::canonical_pair_key(&pool_by_id.pool_token_info);
    assert_eq!(
        crate::state::PAIRS.load(&deps.storage, pair_key).unwrap(),
        pool_id
    );
}

#[test]
fn test_asset() {
    let native_asset = TokenInfo {
        info: TokenType::Native {
            denom: "ubluechip".to_string(),
        },
        amount: Uint128::new(100),
    };

    let token_asset = TokenInfo {
        info: TokenType::CreatorToken {
            denom: String::from("bluechip..."),
        },
        amount: Uint128::new(100),
    };

    assert!(native_asset.is_native_token());
    // Post-migration: creator token is a native TokenFactory denom, so
    // `is_native_token()` now returns true for the CreatorToken variant too.
    assert!(token_asset.is_native_token());
}

#[test]
fn test_config() {
    let config = FactoryInstantiate {
        factory_admin_address: Addr::unchecked("admin1..."),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 1,
        create_pool_wasm_contract_id: 1,
        bluechip_wallet_address: Addr::unchecked("bluechip1..."),
        commit_fee_bluechip: Decimal::percent(10),
        commit_fee_creator: Decimal::percent(10),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    assert_eq!(config.factory_admin_address, Addr::unchecked("admin1..."));
    assert_eq!(config.cw20_token_contract_id, 1);
    assert_eq!(config.create_pool_wasm_contract_id, 1);
    assert_eq!(
        config.bluechip_wallet_address,
        Addr::unchecked("bluechip1...")
    );
    assert_eq!(config.commit_fee_bluechip, Decimal::percent(10));
    assert_eq!(config.commit_fee_creator, Decimal::percent(10));
}

#[allow(deprecated)]
#[test]
fn test_reply_handling() {
    let mut deps = mock_dependencies(&[]);

    let the_admin = addr0000();
    let msg = FactoryInstantiate {
        factory_admin_address: the_admin.clone(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(100),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::from_ratio(10u128, 100u128),
        commit_fee_creator: Decimal::from_ratio(10u128, 100u128),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };

    let env = mock_env();
    let info = message_info(&the_admin, &[]);

    let _res = instantiate(deps.as_mut(), env.clone(), info, msg).unwrap();

    let pool_id = 1u64;

    // Create the pool message
    let pool_msg = CreatePool {
        pool_token_info: [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::CreatorToken {
                denom: String::from("WILL_BE_CREATED_BY_FACTORY"), // Use placeholder
            },
        ],
    };

    // The creation context arrives in the Reply payload, exactly as the
    // create handler attaches it to the pool-instantiate SubMsg. Phase-2:
    // the reply chain is a single step — the pool-instantiate reply
    // (reply id FINALIZE_POOL) finalizes and registers the pool.
    let temp = TempPoolCreation {
        pool_id,
        temp_creator_wallet: the_admin.clone(),
        temp_pool_info: pool_msg,
        subdenom: "test".to_string(),
    };

    let contract_addr_obj = make_addr("pool_contract_address");
    let contract_addr = contract_addr_obj.as_str();

    // Create the pool-instantiate reply with pool_id encoded in the reply ID.
    let reply_msg = Reply {
        id: encode_reply_id(pool_id, FINALIZE_POOL),
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![
                Event::new("instantiate").add_attribute("_contract_address", contract_addr)
            ],
            msg_responses: vec![],
            data: None,
        }),
        gas_used: 0,
        payload: to_json_binary(&temp).unwrap(),
    };

    let res = pool_creation_reply(deps.as_mut(), env.clone(), reply_msg).unwrap();

    assert_eq!(res.attributes.len(), 3);
    assert_eq!(res.attributes[0], ("action", "pool_created_successfully"));
    assert_eq!(res.attributes[1], ("pool_address", contract_addr));
    assert_eq!(res.attributes[2], ("pool_id", "1"));
    // finalize_pool registers only; no outgoing messages.
    assert!(res.messages.is_empty());

    // The pool is now registered with the reconstructed pool-owned creator
    // denom `factory/{pool_addr}/{subdenom}`.
    let pool_by_id = POOLS_BY_ID.load(&deps.storage, pool_id).unwrap();
    assert_eq!(pool_by_id.pool_id, pool_id);
    assert_eq!(pool_by_id.creator_pool_addr, contract_addr_obj);
    assert_eq!(
        pool_by_id.pool_token_info[1],
        TokenType::CreatorToken {
            denom: format!("factory/{}/test", contract_addr),
        }
    );
}

// ---------------------------------------------------------------------------
// NotifyThresholdCrossed — pure registry recording
// ---------------------------------------------------------------------------
// The handler records the crossing exactly once per pool: auth check
// (only the registered pool contract, Commit kind), idempotency gate,
// then a flag save. No messages are emitted.

#[test]
fn test_notify_threshold_crossed_records_flag_and_rejects_duplicate() {
    let mut deps = mock_dependencies(&[]);
    let env = mock_env();
    instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin_addr(), &[]),
        create_default_instantiate_msg(),
    )
    .unwrap();

    let pool_addr = make_addr("pool_contract_1");
    register_test_pool_addr(deps.as_mut().storage, 1, &pool_addr);

    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&pool_addr, &[]),
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id: 1,
            crossed_at: None,
        },
    )
    .unwrap();

    // Pure registry recording: no mint, no bounty — no messages at all.
    assert!(
        res.messages.is_empty(),
        "NotifyThresholdCrossed must not emit any messages, got {}",
        res.messages.len()
    );
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "action" && a.value == "threshold_crossed"));
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "pool_id" && a.value == "1"));
    // crossed_at: None falls back to env.block.time.
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "crossed_at" && a.value == env.block.time.to_string()));

    assert!(
        crate::state::POOL_THRESHOLD_CROSSED
            .load(&deps.storage, 1)
            .unwrap(),
        "crossing flag must be recorded"
    );

    // Idempotency gate: a retried notify is rejected with the dedicated
    // error string so the pool's retry machinery can tell "already
    // recorded" apart from a transient failure.
    let err = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&pool_addr, &[]),
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id: 1,
            crossed_at: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{}", err).contains("Threshold crossing already recorded for this pool"),
        "got: {}",
        err
    );
}

#[test]
fn test_notify_threshold_crossed_records_supplied_crossed_at() {
    let mut deps = mock_dependencies(&[]);
    let env = mock_env();
    instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin_addr(), &[]),
        create_default_instantiate_msg(),
    )
    .unwrap();

    let pool_addr = make_addr("pool_contract_1");
    register_test_pool_addr(deps.as_mut().storage, 1, &pool_addr);

    // The pool-supplied timestamp is recorded verbatim (it may differ
    // from env.block.time, e.g. a retried notify after a transient
    // failure still records the original crossing time).
    let crossed_at = env.block.time.minus_seconds(42);
    let res = execute(
        deps.as_mut(),
        env,
        message_info(&pool_addr, &[]),
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id: 1,
            crossed_at: Some(crossed_at),
        },
    )
    .unwrap();
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "crossed_at" && a.value == crossed_at.to_string()));
}

#[test]
fn test_notify_threshold_crossed_rejects_wrong_caller() {
    let mut deps = mock_dependencies(&[]);
    let env = mock_env();
    instantiate(
        deps.as_mut(),
        env.clone(),
        message_info(&admin_addr(), &[]),
        create_default_instantiate_msg(),
    )
    .unwrap();

    let pool_addr = make_addr("pool_contract_1");
    register_test_pool_addr(deps.as_mut().storage, 1, &pool_addr);

    // Unregistered pool_id -> registry miss.
    let err = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&pool_addr, &[]),
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id: 99,
            crossed_at: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{}", err).contains("not found in registry"),
        "got: {}",
        err
    );

    // Caller that isn't the registered pool contract -> auth failure,
    // and the flag must NOT be recorded.
    let interloper = make_addr("not_the_pool");
    let err = execute(
        deps.as_mut(),
        env,
        message_info(&interloper, &[]),
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id: 1,
            crossed_at: None,
        },
    )
    .unwrap_err();
    assert!(
        format!("{}", err)
            .contains("Only the registered pool contract can notify threshold crossed"),
        "got: {}",
        err
    );
    assert!(crate::state::POOL_THRESHOLD_CROSSED
        .may_load(&deps.storage, 1)
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------------------
// Creator token name/symbol validation
// ---------------------------------------------------------------------------
// These tests exercise validate_creator_token_info directly against every
// rule and both boundaries. They exist to pin the spec: accidental weakening
// of any rule (e.g. allowing lowercase symbols) would break a test here.

use crate::execute::pool_lifecycle::create::validate_creator_token_info;

fn valid_token_info() -> CreatorTokenInfo {
    CreatorTokenInfo {
        name: "Valid Name".to_string(),
        symbol: "VLD".to_string(),
        decimal: 6,
    }
}

#[test]
fn test_validate_accepts_known_good() {
    // Sanity check: the baseline fixture must pass so negative tests
    // below only fail on the specific field they mutate.
    assert!(validate_creator_token_info(&valid_token_info()).is_ok());
}

#[test]
fn test_validate_rejects_wrong_decimals() {
    for bad_decimal in [0u8, 1, 5, 7, 18, 255] {
        let mut info = valid_token_info();
        info.decimal = bad_decimal;
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("decimals must be 6"),
            "decimal={} should be rejected, got: {}",
            bad_decimal,
            err
        );
    }
}

#[test]
fn test_validate_name_length_boundaries() {
    // Name must be 3..=50 inclusive.
    let cases: &[(usize, bool)] = &[
        (0, false), // empty
        (1, false),
        (2, false), // just below min
        (3, true),  // exactly min
        (4, true),
        (25, true),
        (49, true),
        (50, true),  // exactly max
        (51, false), // just above max
        (100, false),
    ];
    for (len, should_pass) in cases {
        let mut info = valid_token_info();
        info.name = "A".repeat(*len);
        let result = validate_creator_token_info(&info);
        assert_eq!(
            result.is_ok(),
            *should_pass,
            "name len={} should be {}",
            len,
            if *should_pass { "accepted" } else { "rejected" }
        );
    }
}

#[test]
fn test_validate_name_rejects_non_ascii() {
    // Non-ASCII should be rejected — common spoofing vector (Cyrillic
    // lookalikes, fullwidth chars, etc.).
    let bad_names = [
        "Nameе",      // trailing Cyrillic 'e'
        "名前テスト", // CJK
        "Pool🚀",     // emoji
        "Café",       // accented Latin
        "Ｔｅｓｔ",   // fullwidth ASCII
    ];
    for name in bad_names {
        let mut info = valid_token_info();
        info.name = name.to_string();
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("printable ASCII"),
            "name '{}' should be rejected, got: {}",
            name,
            err
        );
    }
}

#[test]
fn test_validate_name_rejects_control_chars() {
    for control in ['\n', '\t', '\r', '\0', '\x7f'] {
        let mut info = valid_token_info();
        info.name = format!("Bad{}Name", control);
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("printable ASCII"),
            "control char {:?} should be rejected, got: {}",
            control,
            err
        );
    }
}

#[test]
fn test_validate_name_accepts_printable_ascii() {
    // Spaces, punctuation, digits — all printable ASCII must pass.
    let good_names = [
        "ABC",
        "My Token v2",
        "Pool #42",
        "100% Fair",
        "Token (beta)",
        "A.B.C",
        "a-b-c",
    ];
    for name in good_names {
        let mut info = valid_token_info();
        info.name = name.to_string();
        assert!(
            validate_creator_token_info(&info).is_ok(),
            "name '{}' should be accepted",
            name
        );
    }
}

#[test]
fn test_validate_symbol_length_boundaries() {
    // Symbol must be 3..=12 inclusive.
    let cases: &[(usize, bool)] = &[
        (0, false),
        (1, false),
        (2, false),
        (3, true),
        (6, true),
        (11, true),
        (12, true),
        (13, false),
        (50, false),
    ];
    for (len, should_pass) in cases {
        let mut info = valid_token_info();
        info.symbol = "A".repeat(*len);
        let result = validate_creator_token_info(&info);
        assert_eq!(
            result.is_ok(),
            *should_pass,
            "symbol len={} should be {}",
            len,
            if *should_pass { "accepted" } else { "rejected" }
        );
    }
}

#[test]
fn test_validate_symbol_rejects_lowercase() {
    let bad_symbols = ["abc", "Abc", "ABc", "ABCd", "vld"];
    for symbol in bad_symbols {
        let mut info = valid_token_info();
        info.symbol = symbol.to_string();
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("uppercase"),
            "symbol '{}' should be rejected, got: {}",
            symbol,
            err
        );
    }
}

#[test]
fn test_validate_symbol_rejects_special_chars() {
    // Symbol allows only A-Z and 0-9. Everything else must fail.
    // All strings here are length 3-12 so we only test charset rejection,
    // not length rejection.
    let bad_symbols = ["A.B", "A-B", "A B", "A$B", "A_B", "A@B", "AB!", "AB#"];
    for symbol in bad_symbols {
        let mut info = valid_token_info();
        info.symbol = symbol.to_string();
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("uppercase"),
            "symbol '{}' should be rejected, got: {}",
            symbol,
            err
        );
    }
}

#[test]
fn test_validate_symbol_rejects_non_ascii() {
    let bad_symbols = ["ABCЕ", "ТЕСТ", "A🚀B"];
    for symbol in bad_symbols {
        let mut info = valid_token_info();
        info.symbol = symbol.to_string();
        let err = validate_creator_token_info(&info).unwrap_err();
        assert!(
            format!("{}", err).contains("uppercase"),
            "symbol '{}' should be rejected, got: {}",
            symbol,
            err
        );
    }
}

#[test]
fn test_validate_symbol_accepts_uppercase_and_digits() {
    let good_symbols = [
        "ABC",
        "USDC",
        "BTC",
        "ETH2",
        "USD1",
        "AAA123",
        "AAAAAAAAAAAA",
    ];
    for symbol in good_symbols {
        let mut info = valid_token_info();
        info.symbol = symbol.to_string();
        assert!(
            validate_creator_token_info(&info).is_ok(),
            "symbol '{}' should be accepted",
            symbol
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Factory's pool_token_info pre-instantiate validator
//
// Catches malformed pair specs at CreatePool entry (before any wasm
// instantiate is dispatched) so the downstream pool never sees a
// reversed pair, a wrong-denom bluechip, or a non-sentinel
// creator-token address.
// ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod validate_pool_token_info_tests {
    use crate::asset::TokenType;
    use crate::execute::pool_lifecycle::create::{
        validate_pool_token_info, CREATOR_TOKEN_SENTINEL,
    };

    const CANON: &str = "ubluechip";

    fn good_pair() -> [TokenType; 2] {
        [
            TokenType::Native {
                denom: CANON.to_string(),
            },
            TokenType::CreatorToken {
                denom: String::from(CREATOR_TOKEN_SENTINEL),
            },
        ]
    }

    #[test]
    fn accepts_canonical_pair() {
        validate_pool_token_info(&good_pair(), CANON).expect("canonical pair must validate");
    }

    #[test]
    fn rejects_wrong_bluechip_denom() {
        let mut p = good_pair();
        p[0] = TokenType::Native {
            denom: "uatom".to_string(),
        };
        let err = validate_pool_token_info(&p, CANON).unwrap_err();
        assert!(
            format!("{}", err).contains("must match the factory canonical denom"),
            "got: {}",
            err
        );
    }

    #[test]
    fn rejects_empty_bluechip_denom() {
        let mut p = good_pair();
        p[0] = TokenType::Native {
            denom: "   ".to_string(),
        };
        let err = validate_pool_token_info(&p, CANON).unwrap_err();
        assert!(
            format!("{}", err).contains("Bluechip denom must be non-empty"),
            "got: {}",
            err
        );
    }

    #[test]
    fn rejects_reversed_pair() {
        let mut p = good_pair();
        p.swap(0, 1);
        let err = validate_pool_token_info(&p, CANON).unwrap_err();
        let s = format!("{}", err);
        assert!(
            s.contains("pool_token_info must be") || s.contains("order matters"),
            "got: {}",
            err
        );
    }

    #[test]
    fn rejects_two_creator_tokens() {
        let p = [
            TokenType::CreatorToken {
                denom: String::from(CREATOR_TOKEN_SENTINEL),
            },
            TokenType::CreatorToken {
                denom: String::from(CREATOR_TOKEN_SENTINEL),
            },
        ];
        let err = validate_pool_token_info(&p, CANON).unwrap_err();
        let s = format!("{}", err);
        assert!(
            s.contains("pool_token_info must be") || s.contains("order matters"),
            "got: {}",
            err
        );
    }

    #[test]
    fn rejects_two_native_legs() {
        let p = [
            TokenType::Native {
                denom: CANON.to_string(),
            },
            TokenType::Native {
                denom: "uatom".to_string(),
            },
        ];
        let err = validate_pool_token_info(&p, CANON).unwrap_err();
        let s = format!("{}", err);
        assert!(
            s.contains("pool_token_info must be") || s.contains("order matters"),
            "got: {}",
            err
        );
    }

    // Post phase-1 migration: the validator NO LONGER requires the
    // CreatorToken slot to equal `CREATOR_TOKEN_SENTINEL`. The pool now
    // creates its own TokenFactory denom at instantiate and the factory
    // overwrites the placeholder in `finalize_pool`, so any denom in the
    // index-1 placeholder is accepted here. (Reinterpreted from the old
    // `rejects_creator_token_addr_not_sentinel`.)
    #[test]
    fn accepts_any_creator_token_placeholder_denom() {
        let mut p = good_pair();
        p[1] = TokenType::CreatorToken {
            denom: String::from("a_real_cw20_address"),
        };
        validate_pool_token_info(&p, CANON)
            .expect("placeholder creator-token denom is ignored and must validate");
    }
}

// TODO(phase1-migration): the factory no longer instantiates a CW20 for
// the creator token (it is now a pool-owned TokenFactory native denom), so
// there is no CW20 marketing block to assert. This test pinned CW20
// marketing-admin behavior that no longer exists; it needs to be rewritten
// against the pool-side TokenFactory denom metadata (if/when that carries
// marketing) or deleted. Kept compiling but ignored for now.
#[ignore = "phase1-migration: creator token is now a native TokenFactory denom; no CW20 instantiate to assert marketing on"]
#[test]
fn create_pair_sets_marketing_admin_to_creator() {
    let mut deps = mock_dependencies(&[]);

    let the_admin = addr0000();
    let msg = FactoryInstantiate {
        factory_admin_address: the_admin.clone(),
        cw721_nft_contract_id: 58,
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        bluechip_wallet_address: ubluechip_addr(),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        pool_creation_fee: cosmwasm_std::Uint128::new(1_000_000),
        gamm_pool_creation_fee: cosmwasm_std::Coin {
            denom: String::new(),
            amount: Uint128::zero(),
        },
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    };
    instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&the_admin, &[]),
        msg,
    )
    .unwrap();

    let creator = make_addr("creator0001");
    let res = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&creator, &creation_fee_funds()),
        ExecuteMsg::Create {
            pool_msg: CreatePool {
                pool_token_info: [
                    TokenType::Native {
                        denom: "ubluechip".to_string(),
                    },
                    TokenType::CreatorToken {
                        denom: String::from("WILL_BE_CREATED_BY_FACTORY"),
                    },
                ],
            },
            token_info: CreatorTokenInfo {
                name: "Brand Token".to_string(),
                symbol: "BRAND".to_string(),
                decimal: 6,
            },
        },
    )
    .unwrap();

    // The CW20 instantiate submessage must carry a marketing block with
    // the creator as marketing admin — cw20-base permanently locks
    // marketing (no logo / description / project, ever) when this is
    // None at instantiation.
    let token_init = res
        .messages
        .iter()
        .find_map(|sub| match &sub.msg {
            cosmwasm_std::CosmosMsg::Wasm(cosmwasm_std::WasmMsg::Instantiate {
                code_id,
                msg,
                ..
            }) if *code_id == 10 => {
                Some(cosmwasm_std::from_json::<crate::msg::TokenInstantiateMsg>(msg).unwrap())
            }
            _ => None,
        })
        .expect("create must instantiate the creator token CW20");

    let marketing = token_init
        .marketing
        .expect("marketing must be set at instantiate or it is locked forever");
    assert_eq!(marketing.marketing, Some(creator.to_string()));
    assert_eq!(marketing.project, None);
    assert_eq!(marketing.description, None);
    assert!(marketing.logo.is_none());
}

#[test]
fn pools_query_paginates_registry_in_pool_id_order() {
    let mut deps = mock_dependencies(&[]);

    for pool_id in 1u64..=5 {
        let details = PoolDetails {
            pool_id,
            pool_token_info: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    denom: String::from(format!("token_{pool_id}")),
                },
            ],
            creator_pool_addr: Addr::unchecked(format!("pool_{pool_id}")),
        };
        POOLS_BY_ID
            .save(deps.as_mut().storage, pool_id, &details)
            .unwrap();
    }

    // Full enumeration, ascending by pool_id.
    let all = crate::query::query_pools(deps.as_ref(), None, None).unwrap();
    assert_eq!(all.pools.len(), 5);
    assert_eq!(
        all.pools.iter().map(|p| p.pool_id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(all.pools[0].pool_addr, Addr::unchecked("pool_1"));

    // Page 1.
    let page1 = crate::query::query_pools(deps.as_ref(), None, Some(2)).unwrap();
    assert_eq!(
        page1.pools.iter().map(|p| p.pool_id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    // Page 2 resumes after the last seen id.
    let page2 = crate::query::query_pools(deps.as_ref(), Some(2), Some(2)).unwrap();
    assert_eq!(
        page2.pools.iter().map(|p| p.pool_id).collect::<Vec<_>>(),
        vec![3, 4]
    );
    // Past the end: empty page signals end-of-data.
    let page4 = crate::query::query_pools(deps.as_ref(), Some(5), Some(2)).unwrap();
    assert!(page4.pools.is_empty());

    // Limit is clamped to the max page size.
    let clamped = crate::query::query_pools(deps.as_ref(), None, Some(10_000)).unwrap();
    assert_eq!(clamped.pools.len(), 5);
}

// Pins the instantiate-message shape emitted by ./deploy_osmosis.sh to
// the FactoryInstantiate struct. If a field is renamed or a new
// required (non-defaulted) field is added, this fails and the deploy
// script must be updated in the same change.
#[test]
fn deploy_script_instantiate_json_deserializes() {
    let json = r#"{
        "factory_admin_address": "osmo1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
        "bluechip_wallet_address": "osmo1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
        "bluechip_denom": "uosmo",
        "usd_quote_denom": "ibc/498A0751C798A0D9A389AA3691123DADA57DAA4FE165D5C75894505B876BA6E4",
        "pricing_pool_id": 1,
        "twap_window_seconds": 600,
        "commit_threshold_limit_usd": "25000000000",
        "commit_fee_bluechip": "0.01",
        "commit_fee_creator": "0.05",
        "max_bluechip_lock_per_pool": "25000000000",
        "creator_excess_liquidity_lock_days": 7,
        "pool_creation_fee": "1000000",
        "emergency_withdraw_delay_seconds": 86400,
        "cw20_token_contract_id": 1,
        "cw721_nft_contract_id": 2,
        "create_pool_wasm_contract_id": 3
    }"#;
    let msg: FactoryInstantiate =
        cosmwasm_std::from_json(json.as_bytes()).expect("deploy script JSON must deserialize");
    assert_eq!(msg.bluechip_denom, "uosmo");
    assert_eq!(msg.pricing_pool_id, 1);
    assert_eq!(msg.commit_threshold_limit_usd, Uint128::new(25_000_000_000));
    // Omitted-with-default field must land on the canonical payout split.
    msg.threshold_payout_amounts
        .validate()
        .expect("defaulted threshold_payout_amounts must be canonical");
}
