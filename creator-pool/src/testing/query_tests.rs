use cosmwasm_std::{
    from_json,
    testing::{mock_dependencies, mock_env, MockApi},
    Addr, Decimal, Timestamp, Uint128,
};

use crate::asset::{TokenInfo, TokenType};
use crate::msg::{
    CommitStatus, CreatorEarningsResponse, FeeInfoResponse, LastCommittedResponse,
    PoolFeeStateResponse, PoolInfoResponse, PoolStateResponse, QueryMsg,
};
use crate::query::query;
use crate::state::{
    Committing, CreatorExcessLiquidity, COMMIT_INFO, CREATOR_EXCESS_POSITION, THRESHOLD_CROSSED_AT,
    USD_RAISED_FROM_COMMIT,
};
use crate::testing::fixtures::{setup_pool_post_threshold, setup_pool_storage};

#[test]
fn test_query_native_pool_id_pre_and_post_threshold() {
    use crate::msg::NativePoolIdResponse;

    // Pre-threshold: no native pool yet -> both fields None.
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    let resp: NativePoolIdResponse = from_json(
        query(deps.as_ref(), mock_env(), QueryMsg::NativePoolId {}).unwrap(),
    )
    .unwrap();
    assert_eq!(resp.pool_id, None);
    assert_eq!(resp.lp_share_denom, None);

    // Post-threshold: fixture stores POOL_ID = 1 -> id + gamm/pool/1.
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);
    let resp: NativePoolIdResponse = from_json(
        query(deps.as_ref(), mock_env(), QueryMsg::NativePoolId {}).unwrap(),
    )
    .unwrap();
    assert_eq!(resp.pool_id, Some(1));
    assert_eq!(resp.lp_share_denom.as_deref(), Some("gamm/pool/1"));
}

#[test]
fn test_query_simulation_wrong_asset() {
    // The pair-membership check fires before any native-pool estimate
    // query, so an unknown asset is rejected cleanly even under the mock.
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
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
fn test_query_pool_info() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::PoolInfo {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let info: PoolInfoResponse = from_json(res).unwrap();

    // Phase-2: reserves are read from the native pool; the mock querier
    // cannot answer gamm queries, so they fail-soft to zero.
    assert_eq!(info.pool_state.reserve0, Uint128::zero());
    assert_eq!(info.pool_state.reserve1, Uint128::zero());
    assert!(!info.pool_state.nft_ownership_accepted);
    assert_eq!(info.total_positions, 0);

    // Internal fee-growth accounting is gone; reported as zero.
    assert_eq!(info.fee_state.fee_growth_global_0, Decimal::zero());
    assert_eq!(info.fee_state.total_fees_collected_0, Uint128::zero());
}

#[test]
fn test_query_fee_state() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = QueryMsg::FeeState {};
    let res = query(deps.as_ref(), env, msg).unwrap();
    let resp: PoolFeeStateResponse = from_json(res).unwrap();

    // Internal fee accounting removed — always zero now.
    assert_eq!(resp.fee_growth_global_0, Decimal::zero());
    assert_eq!(resp.fee_growth_global_1, Decimal::zero());
    assert_eq!(resp.total_fees_collected_0, Uint128::zero());
    assert_eq!(resp.total_fees_collected_1, Uint128::zero());
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
    assert_eq!(resp.last_payment_bluechip, Some(Uint128::new(1_000_000_000)));
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

    // Reserves are native-queried; the mock can't answer, so 0.
    assert_eq!(state.reserve0, Uint128::zero());
    assert_eq!(state.reserve1, Uint128::zero());
    assert!(!state.nft_ownership_accepted);
    assert_eq!(state.total_liquidity, Uint128::zero());
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
    // No excess position recorded.
    assert!(resp.excess.is_none());
    assert!(resp.is_threshold_hit);
    // Fixture never records the crossing timestamp.
    assert!(resp.threshold_crossed_at.is_none());
}

#[test]
fn test_query_creator_earnings_locked_excess() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

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
            },
        )
        .unwrap();
    THRESHOLD_CROSSED_AT
        .save(&mut deps.storage, &env.block.time.minus_seconds(3_600))
        .unwrap();

    // Still locked: one day before unlock_time.
    let res = query(deps.as_ref(), env.clone(), QueryMsg::CreatorEarnings {}).unwrap();
    let resp: CreatorEarningsResponse = from_json(res).unwrap();

    let excess = resp.excess.expect("excess position should be reported");
    assert_eq!(excess.bluechip_amount, Uint128::new(15_000_000_000));
    assert_eq!(excess.token_amount, Uint128::new(30_000_000_000));
    assert_eq!(excess.unlock_time, unlock);
    assert!(!excess.claimable_now);
    assert_eq!(
        resp.threshold_crossed_at,
        Some(env.block.time.minus_seconds(3_600))
    );

    // At unlock_time exactly, claimable_now must agree with the claim
    // handler's `block.time < unlock_time` gate.
    let mut late_env = mock_env();
    late_env.block.time = unlock;
    let res = query(deps.as_ref(), late_env, QueryMsg::CreatorEarnings {}).unwrap();
    let resp: CreatorEarningsResponse = from_json(res).unwrap();
    assert!(resp.excess.expect("excess still unclaimed").claimable_now);
}

#[test]
fn last_commited_query_accepts_both_spellings() {
    // The canonical wire name has a typo ("last_commited"); the serde
    // alias must also accept the correct spelling.
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
