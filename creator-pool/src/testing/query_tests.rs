use cosmwasm_std::{
    from_json,
    testing::{mock_dependencies, mock_env, MockApi, MockStorage},
    Addr, Coin, Decimal, OwnedDeps, Timestamp, Uint128,
};
use std::str::FromStr;

use crate::asset::{TokenInfo, TokenType};
use crate::mock_querier;
use crate::msg::{
    CommitStatus, CreatorEarningsResponse, FeeInfoResponse, LastCommittedResponse,
    PoolFeeStateResponse, PoolInfoResponse, PoolStateResponse, PositionsResponse, QueryMsg,
    ReverseSimulationResponse, SimulationResponse,
};
use crate::query::query;
use crate::state::{
    Committing, CreatorExcessLiquidity, CreatorFeePot, COMMIT_INFO, CREATOR_EXCESS_POSITION,
    CREATOR_FEE_POT, NEXT_POSITION_ID, OWNER_POSITIONS, POOL_FEE_STATE, THRESHOLD_CROSSED_AT,
    USD_RAISED_FROM_COMMIT,
};
use crate::testing::liquidity_tests::{
    create_test_position, setup_pool_post_threshold, setup_pool_storage,
};

// Setup pool storage on the custom mock querier that supports simulation queries.
// Simulation queries call `query_pools()` which needs bank balance + CW20 balance queries.
fn setup_pool_with_querier() -> OwnedDeps<MockStorage, MockApi, mock_querier::WasmMockQuerier> {
    let mut deps = mock_querier::mock_dependencies(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(23_500_000_000),
    }]);

    // Reuse setup_pool_post_threshold logic but on custom querier deps
    use crate::asset::PoolPairType;
    use crate::msg::CommitFeeInfo;
    use crate::state::*;

    let pool_info = PoolInfo {
        pool_id: 1u64,
        pool_info: PoolDetails {
            asset_infos: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    contract_addr: Addr::unchecked("token_contract"),
                },
            ],
            contract_addr: Addr::unchecked(cosmwasm_std::testing::MOCK_CONTRACT_ADDR),
            pool_type: PoolPairType::Xyk {},
        },
        factory_addr: Addr::unchecked("factory"),
        token_address: Addr::unchecked("token_contract"),
        position_nft_address: Addr::unchecked("nft_contract"),
    };
    POOL_INFO.save(&mut deps.storage, &pool_info).unwrap();

    let pool_state = PoolState {
        pool_contract_address: Addr::unchecked(cosmwasm_std::testing::MOCK_CONTRACT_ADDR),
        nft_ownership_accepted: true,
        reserve0: Uint128::new(23_500_000_000),
        reserve1: Uint128::new(350_000_000_000),
        total_liquidity: Uint128::new(91_104_335_791),
        block_time_last: 1_600_000_000,
        price0_cumulative_last: Uint128::zero(),
        price1_cumulative_last: Uint128::zero(),
    };
    POOL_STATE.save(&mut deps.storage, &pool_state).unwrap();

    let pool_fee_state = PoolFeeState {
        fee_growth_global_0: Decimal::zero(),
        fee_growth_global_1: Decimal::zero(),
        total_fees_collected_0: Uint128::zero(),
        total_fees_collected_1: Uint128::zero(),
        fee_reserve_0: Uint128::zero(),
        fee_reserve_1: Uint128::zero(),
    };
    POOL_FEE_STATE
        .save(&mut deps.storage, &pool_fee_state)
        .unwrap();

    let pool_specs = PoolSpecs {
        lp_fee: Decimal::percent(3) / Uint128::new(10),
        min_commit_interval: 60,
    };
    POOL_SPECS.save(&mut deps.storage, &pool_specs).unwrap();

    let commit_config = CommitLimitInfo {
        commit_amount_for_threshold: Uint128::new(25_000_000_000),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        min_commit_pre_threshold: crate::state::DEFAULT_MIN_COMMIT_PRE_THRESHOLD,
        min_commit_post_threshold: crate::state::DEFAULT_MIN_COMMIT_POST_THRESHOLD,
    };
    COMMIT_LIMIT_INFO
        .save(&mut deps.storage, &commit_config)
        .unwrap();

    let commit_fee_info = CommitFeeInfo {
        bluechip_wallet_address: Addr::unchecked("bluechip_treasury"),
        creator_wallet_address: Addr::unchecked("creator_wallet"),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
    };
    COMMITFEEINFO
        .save(&mut deps.storage, &commit_fee_info)
        .unwrap();

    IS_THRESHOLD_HIT.save(&mut deps.storage, &true).unwrap();
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(25_000_000_000))
        .unwrap();
    NEXT_POSITION_ID.save(&mut deps.storage, &1u64).unwrap();

    // Seed CW20 balances so query_pools() works
    deps.querier.with_token_balances(&[(
        &"token_contract".to_string(),
        &[(
            &cosmwasm_std::testing::MOCK_CONTRACT_ADDR.to_string(),
            &Uint128::new(350_000_000_000),
        )],
    )]);

    deps
}

#[test]
fn test_query_simulation_bluechip_to_token() {
    let deps = setup_pool_with_querier();

    let env = mock_env();

    // Simulate swapping 1k bluechip for creator tokens
    let offer = TokenInfo {
        info: TokenType::Native {
            denom: "ubluechip".to_string(),
        },
        amount: Uint128::new(1_000_000_000),
    };

    let msg = QueryMsg::Simulation { offer_asset: offer };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let sim: SimulationResponse = from_json(res).unwrap();

    // With 23.5k bluechip and 350k token reserves:
    // return_amount should be positive
    assert!(
        sim.return_amount > Uint128::zero(),
        "return_amount should be > 0"
    );
    // spread should exist
    assert!(
        sim.spread_amount > Uint128::zero(),
        "spread_amount should be > 0"
    );
    // commission should exist (0.3% fee)
    assert!(
        sim.commission_amount > Uint128::zero(),
        "commission_amount should be > 0"
    );
    // return_amount + spread + commission should approximate the "ideal" swap output
    let total = sim.return_amount + sim.spread_amount + sim.commission_amount;
    assert!(total > Uint128::zero());
}

#[test]
fn test_query_simulation_token_to_bluechip() {
    let deps = setup_pool_with_querier();

    let env = mock_env();

    // Simulate swapping creator tokens for bluechip
    let offer = TokenInfo {
        info: TokenType::CreatorToken {
            contract_addr: Addr::unchecked("token_contract"),
        },
        amount: Uint128::new(10_000_000_000), // 10k tokens
    };

    let msg = QueryMsg::Simulation { offer_asset: offer };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let sim: SimulationResponse = from_json(res).unwrap();

    assert!(sim.return_amount > Uint128::zero());
    assert!(sim.commission_amount > Uint128::zero());
}

#[test]
fn test_query_simulation_wrong_asset() {
    let deps = setup_pool_with_querier();

    let env = mock_env();

    // Unknown asset should fail
    let offer = TokenInfo {
        info: TokenType::Native {
            denom: "uatom".to_string(),
        }, // wrong denom
        amount: Uint128::new(1_000_000_000),
    };

    let msg = QueryMsg::Simulation { offer_asset: offer };
    let err = query(deps.as_ref(), env, msg).unwrap_err();
    assert!(err.to_string().contains("does not belong"));
}

#[test]
fn test_query_reverse_simulation() {
    let deps = setup_pool_with_querier();

    let env = mock_env();

    // "I want 5k creator tokens, how much bluechip do I need?"
    let ask = TokenInfo {
        info: TokenType::CreatorToken {
            contract_addr: Addr::unchecked("token_contract"),
        },
        amount: Uint128::new(5_000_000_000),
    };

    let msg = QueryMsg::ReverseSimulation { ask_asset: ask };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let rsim: ReverseSimulationResponse = from_json(res).unwrap();

    assert!(
        rsim.offer_amount > Uint128::zero(),
        "offer_amount should be > 0"
    );
    assert!(
        rsim.spread_amount > Uint128::zero(),
        "spread_amount should be > 0"
    );
    assert!(
        rsim.commission_amount > Uint128::zero(),
        "commission_amount should be > 0"
    );
}

#[test]
fn test_query_positions_by_owner() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let alice = MockApi::default().addr_make("alice");
    let bob = MockApi::default().addr_make("bob");
    let charlie = MockApi::default().addr_make("charlie");

    // Create positions for different owners
    create_test_position(&mut deps, 1, alice.as_str(), Uint128::new(1_000_000));
    create_test_position(&mut deps, 2, bob.as_str(), Uint128::new(2_000_000));
    create_test_position(&mut deps, 3, alice.as_str(), Uint128::new(3_000_000));
    create_test_position(&mut deps, 4, charlie.as_str(), Uint128::new(500_000));

    // Register in OWNER_POSITIONS secondary index
    OWNER_POSITIONS
        .save(&mut deps.storage, (&alice, "1"), &true)
        .unwrap();
    OWNER_POSITIONS
        .save(&mut deps.storage, (&bob, "2"), &true)
        .unwrap();
    OWNER_POSITIONS
        .save(&mut deps.storage, (&alice, "3"), &true)
        .unwrap();
    OWNER_POSITIONS
        .save(&mut deps.storage, (&charlie, "4"), &true)
        .unwrap();

    let env = mock_env();

    // Query Alice's positions
    let msg = QueryMsg::PositionsByOwner {
        owner: MockApi::default().addr_make("alice").to_string(),
        start_after: None,
        limit: None,
    };
    let res = query(deps.as_ref(), env.clone(), msg).unwrap();
    let positions: PositionsResponse = from_json(res).unwrap();

    assert_eq!(
        positions.positions.len(),
        2,
        "Alice should have 2 positions"
    );

    // Verify both are Alice's
    for pos in &positions.positions {
        assert_eq!(pos.owner, MockApi::default().addr_make("alice"));
    }

    // Query Bob's positions
    let msg = QueryMsg::PositionsByOwner {
        owner: MockApi::default().addr_make("bob").to_string(),
        start_after: None,
        limit: None,
    };
    let res = query(deps.as_ref(), env.clone(), msg).unwrap();
    let positions: PositionsResponse = from_json(res).unwrap();

    assert_eq!(positions.positions.len(), 1, "Bob should have 1 position");
    assert_eq!(
        positions.positions[0].owner,
        MockApi::default().addr_make("bob")
    );
}

#[test]
fn test_query_positions_by_owner_empty() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();

    // Query for user with no positions
    let msg = QueryMsg::PositionsByOwner {
        owner: MockApi::default().addr_make("nobody").to_string(),
        start_after: None,
        limit: None,
    };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let positions: PositionsResponse = from_json(res).unwrap();

    assert_eq!(positions.positions.len(), 0);
}

#[test]
fn test_query_positions_by_owner_pagination() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let alice = MockApi::default().addr_make("alice");

    // Create 5 positions for Alice
    for i in 1..=5 {
        create_test_position(&mut deps, i, alice.as_str(), Uint128::new(1_000_000));
        OWNER_POSITIONS
            .save(&mut deps.storage, (&alice, &i.to_string()), &true)
            .unwrap();
    }

    let env = mock_env();

    // Get first 2
    let msg = QueryMsg::PositionsByOwner {
        owner: MockApi::default().addr_make("alice").to_string(),
        start_after: None,
        limit: Some(2),
    };
    let res = query(deps.as_ref(), env.clone(), msg).unwrap();
    let page1: PositionsResponse = from_json(res).unwrap();
    assert_eq!(page1.positions.len(), 2);

    // Get next page starting after the last position ID from page 1
    let last_id = &page1.positions.last().unwrap().position_id;
    let msg = QueryMsg::PositionsByOwner {
        owner: MockApi::default().addr_make("alice").to_string(),
        start_after: Some(last_id.clone()),
        limit: Some(2),
    };
    let res = query(deps.as_ref(), env.clone(), msg).unwrap();
    let page2: PositionsResponse = from_json(res).unwrap();
    assert_eq!(page2.positions.len(), 2);

    // Verify no overlap between pages
    let page1_ids: Vec<_> = page1.positions.iter().map(|p| &p.position_id).collect();
    let page2_ids: Vec<_> = page2.positions.iter().map(|p| &p.position_id).collect();
    for id in &page2_ids {
        assert!(!page1_ids.contains(id), "Pages should not overlap");
    }
}

#[test]
fn test_query_pool_info() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);
    NEXT_POSITION_ID.save(&mut deps.storage, &5u64).unwrap();

    let env = mock_env();
    let msg = QueryMsg::PoolInfo {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let info: PoolInfoResponse = from_json(res).unwrap();

    assert_eq!(info.pool_state.reserve0, Uint128::new(23_500_000_000));
    assert_eq!(info.pool_state.reserve1, Uint128::new(350_000_000_000));
    assert!(info.pool_state.nft_ownership_accepted);
    assert_eq!(info.total_positions, 5);

    // Fee state should be initialized at zero
    assert_eq!(info.fee_state.fee_growth_global_0, Decimal::zero());
    assert_eq!(info.fee_state.total_fees_collected_0, Uint128::zero());
}

#[test]
fn test_query_fee_state() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    // Inject some fee data
    let mut fee_state = POOL_FEE_STATE.load(&deps.storage).unwrap();
    fee_state.fee_growth_global_0 = Decimal::from_str("12.5").unwrap();
    fee_state.fee_growth_global_1 = Decimal::from_str("0.75").unwrap();
    fee_state.total_fees_collected_0 = Uint128::new(500_000);
    fee_state.total_fees_collected_1 = Uint128::new(750_000);
    POOL_FEE_STATE.save(&mut deps.storage, &fee_state).unwrap();

    let env = mock_env();
    let msg = QueryMsg::FeeState {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let resp: PoolFeeStateResponse = from_json(res).unwrap();

    assert_eq!(resp.fee_growth_global_0, Decimal::from_str("12.5").unwrap());
    assert_eq!(resp.fee_growth_global_1, Decimal::from_str("0.75").unwrap());
    assert_eq!(resp.total_fees_collected_0, Uint128::new(500_000));
    assert_eq!(resp.total_fees_collected_1, Uint128::new(750_000));
}

#[test]
fn test_query_fee_info() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::FeeInfo {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let resp: FeeInfoResponse = from_json(res).unwrap();

    assert_eq!(resp.fee_info.commit_fee_bluechip, Decimal::percent(1));
    assert_eq!(resp.fee_info.commit_fee_creator, Decimal::percent(5));
    assert_eq!(
        resp.fee_info.bluechip_wallet_address,
        Addr::unchecked("bluechip_treasury")
    );
    assert_eq!(
        resp.fee_info.creator_wallet_address,
        Addr::unchecked("creator_wallet")
    );
}

#[test]
fn test_query_committing_info_exists() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    let user = MockApi::default().addr_make("committer1");
    COMMIT_INFO
        .save(
            &mut deps.storage,
            &user,
            &Committing {
                pool_contract_address: Addr::unchecked("pool_contract"),
                committer: user.clone(),
                total_paid_usd: Uint128::new(5_000_000_000),
                total_paid_bluechip: Uint128::new(5_000_000_000),
                last_committed: Timestamp::from_seconds(1_600_000_000),
                last_payment_bluechip: Uint128::new(1_000_000_000),
                last_payment_usd: Uint128::new(1_000_000_000),
            },
        )
        .unwrap();

    let env = mock_env();
    let msg = QueryMsg::CommittingInfo {
        wallet: MockApi::default().addr_make("committer1").to_string(),
    };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let info: Option<Committing> = from_json(res).unwrap();

    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.total_paid_usd, Uint128::new(5_000_000_000));
    assert_eq!(info.total_paid_bluechip, Uint128::new(5_000_000_000));
}

#[test]
fn test_query_committing_info_not_found() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::CommittingInfo {
        wallet: MockApi::default().addr_make("nobody").to_string(),
    };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let info: Option<Committing> = from_json(res).unwrap();

    assert!(info.is_none());
}

#[test]
fn test_query_last_committed_exists() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    let user = MockApi::default().addr_make("committer1");
    COMMIT_INFO
        .save(
            &mut deps.storage,
            &user,
            &Committing {
                pool_contract_address: Addr::unchecked("pool_contract"),
                committer: user.clone(),
                total_paid_usd: Uint128::new(5_000_000_000),
                total_paid_bluechip: Uint128::new(5_000_000_000),
                last_committed: Timestamp::from_seconds(1_600_000_000),
                last_payment_bluechip: Uint128::new(1_000_000_000),
                last_payment_usd: Uint128::new(1_000_000_000),
            },
        )
        .unwrap();

    let env = mock_env();
    let msg = QueryMsg::LastCommited {
        wallet: MockApi::default().addr_make("committer1").to_string(),
    };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let resp: LastCommittedResponse = from_json(res).unwrap();

    assert!(resp.has_committed);
    assert_eq!(
        resp.last_committed,
        Some(Timestamp::from_seconds(1_600_000_000))
    );
    assert_eq!(
        resp.last_payment_bluechip,
        Some(Uint128::new(1_000_000_000))
    );
    assert_eq!(resp.last_payment_usd, Some(Uint128::new(1_000_000_000)));
}

#[test]
fn test_query_last_committed_not_found() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::LastCommited {
        wallet: MockApi::default().addr_make("nobody").to_string(),
    };
    let res = query(deps.as_ref(), env, msg).unwrap();
    let resp: LastCommittedResponse = from_json(res).unwrap();

    assert!(!resp.has_committed);
    assert!(resp.last_committed.is_none());
    assert!(resp.last_payment_bluechip.is_none());
}

#[test]
fn test_query_is_fully_commited_in_progress() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    // Pool not yet at threshold
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(10_000_000_000))
        .unwrap();

    let env = mock_env();
    let msg = QueryMsg::IsFullyCommited {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let status: CommitStatus = from_json(res).unwrap();

    match status {
        CommitStatus::InProgress { raised, target } => {
            assert_eq!(raised, Uint128::new(10_000_000_000));
            assert_eq!(target, Uint128::new(25_000_000_000));
        }
        CommitStatus::FullyCommitted => panic!("Should be InProgress"),
    }
}

#[test]
fn test_query_is_fully_commited_fully_committed() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::IsFullyCommited {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let status: CommitStatus = from_json(res).unwrap();

    assert!(matches!(status, CommitStatus::FullyCommitted));
}

#[test]
fn test_query_pool_state() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::PoolState {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let state: PoolStateResponse = from_json(res).unwrap();

    assert_eq!(state.reserve0, Uint128::new(23_500_000_000));
    assert_eq!(state.reserve1, Uint128::new(350_000_000_000));
    assert!(state.nft_ownership_accepted);
    assert!(state.total_liquidity > Uint128::zero());
}

#[test]
fn test_query_creator_earnings_empty_defaults() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let res = query(deps.as_ref(), mock_env(), QueryMsg::CreatorEarnings {}).unwrap();
    let resp: CreatorEarningsResponse = from_json(res).unwrap();

    assert_eq!(
        resp.creator_wallet_address,
        Addr::unchecked("creator_wallet")
    );
    // No clip-slice fees accrued and no excess position recorded.
    assert_eq!(resp.fee_pot.amount_0, Uint128::zero());
    assert_eq!(resp.fee_pot.amount_1, Uint128::zero());
    assert!(resp.excess.is_none());
    assert!(resp.is_threshold_hit);
    // Fixture never records the crossing timestamp.
    assert!(resp.threshold_crossed_at.is_none());
}

#[test]
fn test_query_creator_earnings_pot_and_locked_excess() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    CREATOR_FEE_POT
        .save(
            &mut deps.storage,
            &CreatorFeePot {
                amount_0: Uint128::new(850_000_000),
                amount_1: Uint128::new(1_200_000_000),
            },
        )
        .unwrap();

    let env = mock_env();
    let unlock = env.block.time.plus_seconds(86_400);
    CREATOR_EXCESS_POSITION
        .save(
            &mut deps.storage,
            &CreatorExcessLiquidity {
                creator: Addr::unchecked("creator_wallet"),
                bluechip_amount: Uint128::new(15_000_000_000),
                token_amount: Uint128::new(30_000_000_000),
                unlock_time: unlock,
                excess_nft_id: None,
            },
        )
        .unwrap();
    THRESHOLD_CROSSED_AT
        .save(&mut deps.storage, &env.block.time.minus_seconds(3_600))
        .unwrap();

    // Still locked: one day before unlock_time.
    let res = query(deps.as_ref(), env.clone(), QueryMsg::CreatorEarnings {}).unwrap();
    let resp: CreatorEarningsResponse = from_json(res).unwrap();

    assert_eq!(resp.fee_pot.amount_0, Uint128::new(850_000_000));
    assert_eq!(resp.fee_pot.amount_1, Uint128::new(1_200_000_000));
    let excess = resp.excess.expect("excess position should be reported");
    assert_eq!(excess.bluechip_amount, Uint128::new(15_000_000_000));
    assert_eq!(excess.token_amount, Uint128::new(30_000_000_000));
    assert_eq!(excess.unlock_time, unlock);
    assert!(!excess.claimable_now);
    assert_eq!(
        resp.threshold_crossed_at,
        Some(env.block.time.minus_seconds(3_600))
    );

    // At unlock_time exactly, the claim handler's `block.time <
    // unlock_time` gate no longer rejects — claimable_now must agree.
    let mut late_env = mock_env();
    late_env.block.time = unlock;
    let res = query(deps.as_ref(), late_env, QueryMsg::CreatorEarnings {}).unwrap();
    let resp: CreatorEarningsResponse = from_json(res).unwrap();
    assert!(resp.excess.expect("excess still unclaimed").claimable_now);
}

#[test]
fn last_commited_query_accepts_both_spellings() {
    // The canonical wire name has a typo ("last_commited"); the serde
    // alias must also accept the correct spelling so new integrations
    // don't have to reproduce it.
    let typo: QueryMsg = from_json(br#"{"last_commited": {"wallet": "bluechip1fan"}}"#).unwrap();
    let correct: QueryMsg =
        from_json(br#"{"last_committed": {"wallet": "bluechip1fan"}}"#).unwrap();
    for msg in [typo, correct] {
        match msg {
            QueryMsg::LastCommited { wallet } => assert_eq!(wallet, "bluechip1fan"),
            other => panic!("expected LastCommited, got {other:?}"),
        }
    }
}

#[test]
fn simulation_prices_against_accounting_reserves_not_balances() {
    // Regression: simulations used to derive reserves from the
    // contract's bank/cw20 BALANCES, which on commit pools also hold
    // LP fee reserves, the creator fee pot, and commit proceeds —
    // inflating quoted depth so execution undershoots the quote. The
    // quote must come from POOL_STATE, the reserves execution trades
    // against.
    let mut deps = setup_pool_with_querier();

    // Drive the accounting reserves away from the bank/cw20 balances the
    // querier was seeded with. If the simulation still read balances,
    // the quote below would not match the POOL_STATE-derived expectation.
    let mut pool_state = crate::state::POOL_STATE.load(&deps.storage).unwrap();
    pool_state.reserve0 = Uint128::new(10_000_000_000);
    pool_state.reserve1 = Uint128::new(100_000_000_000);
    crate::state::POOL_STATE
        .save(&mut deps.storage, &pool_state)
        .unwrap();
    let pool_specs = crate::state::POOL_SPECS.load(&deps.storage).unwrap();
    let offer_amount = Uint128::new(1_000_000_000);
    let (expected_return, expected_spread, expected_commission) = crate::swap_helper::compute_swap(
        pool_state.reserve0,
        pool_state.reserve1,
        offer_amount,
        pool_specs.lp_fee,
    )
    .unwrap();

    let res = query(
        deps.as_ref(),
        mock_env(),
        QueryMsg::Simulation {
            offer_asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: offer_amount,
            },
        },
    )
    .unwrap();
    let resp: SimulationResponse = from_json(res).unwrap();

    assert_eq!(resp.return_amount, expected_return);
    assert_eq!(resp.spread_amount, expected_spread);
    assert_eq!(resp.commission_amount, expected_commission);
}

#[test]
fn simulation_on_zero_reserves_errors_cleanly_instead_of_panicking() {
    // Pre-threshold commit pools have POOL_STATE reserves of 0/0.
    // compute_swap divides by the offer reserve, so without the guard the
    // query PANICS (VM error) rather than returning a decodable error.
    let mut deps = setup_pool_with_querier();
    let mut pool_state = crate::state::POOL_STATE.load(&deps.storage).unwrap();
    pool_state.reserve0 = Uint128::zero();
    pool_state.reserve1 = Uint128::zero();
    crate::state::POOL_STATE
        .save(&mut deps.storage, &pool_state)
        .unwrap();

    let offer = TokenInfo {
        info: TokenType::Native {
            denom: "ubluechip".to_string(),
        },
        amount: Uint128::new(1_000_000),
    };
    let err = query(
        deps.as_ref(),
        mock_env(),
        QueryMsg::Simulation { offer_asset: offer },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("no active liquidity"),
        "expected clean zero-liquidity error, got: {err}"
    );

    let err = query(
        deps.as_ref(),
        mock_env(),
        QueryMsg::ReverseSimulation {
            ask_asset: TokenInfo {
                info: TokenType::CreatorToken {
                    contract_addr: Addr::unchecked("token_contract"),
                },
                amount: Uint128::new(1_000_000),
            },
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("no active liquidity"),
        "expected clean zero-liquidity error, got: {err}"
    );
}
