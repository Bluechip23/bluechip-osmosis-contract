//! Swap / commit / distribution unit tests.
//!
//! Phase-2: the internal constant-product AMM + LP-position system was
//! replaced by a NATIVE Osmosis GAMM pool. A `SimpleSwap` (and a
//! post-threshold `Commit`) no longer does reserve math and `BankMsg::Send`s
//! the output inline; it emits ONE
//! `SubMsg::reply_on_success(MsgSwapExactAmountIn, REPLY_ID_SWAP_FORWARD=4)`
//! and the output `BankMsg::Send` happens later in `contract::reply(id=4)`.
//! Tests that inspected reserves / fee-growth / price accumulators are gone.

use crate::asset::{TokenInfo, TokenType};
use crate::contract::{execute, instantiate};
use crate::error::ContractError;
use crate::generic_helpers::{calculate_effective_batch_size, trigger_threshold_payout};
use crate::msg::{CommitFeeInfo, ExecuteMsg, PoolInstantiateMsg};
use crate::state::{
    DistributionState, SwapForwardPayload, COMMITFEEINFO, COMMIT_INFO, COMMIT_LEDGER,
    COMMIT_LIMIT_INFO, DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION, DEFAULT_MAX_GAS_PER_TX,
    DISTRIBUTION_STATE, IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_INFO, REENTRANCY_LOCK,
    REPLY_ID_SWAP_FORWARD, THRESHOLD_PAYOUT_AMOUNTS, THRESHOLD_PROCESSING, USD_RAISED_FROM_COMMIT,
};
use crate::testing::fixtures::{
    mock_dependencies_with_balance, setup_pool_post_threshold, setup_pool_storage,
    with_factory_oracle, CREATOR_DENOM,
};
use cosmwasm_std::testing::{message_info, mock_dependencies, mock_env, MockApi};
use cosmwasm_std::{
    from_json, Addr, BankMsg, Binary, Coin, CosmosMsg, Decimal, DepsMut,
    MsgResponse, Order, Reply, Response, SubMsgResponse, SubMsgResult, Timestamp, Uint128,
};
use osmosis_std::types::osmosis::poolmanager::v1beta1::MsgSwapExactAmountInResponse;
use prost::Message;

// ---------------------------------------------------------------------------
// Reply helper: drive a `REPLY_ID_SWAP_FORWARD` (id 4) reply carrying the
// given SubMsg `payload` and a mocked `token_out_amount`. Returns the
// `Response` produced by `contract::reply`, whose `BankMsg::Send` forwards
// the swapped-out tokens to the receiver recorded in the payload.
// ---------------------------------------------------------------------------
fn drive_swap_forward_reply(deps: DepsMut, payload: Binary, token_out_amount: u128) -> Response {
    let out = MsgSwapExactAmountInResponse {
        token_out_amount: token_out_amount.to_string(),
    };
    #[allow(deprecated)]
    let reply_msg = Reply {
        id: REPLY_ID_SWAP_FORWARD,
        payload,
        gas_used: 0,
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![],
            data: None,
            msg_responses: vec![MsgResponse {
                type_url: "/osmosis.poolmanager.v1beta1.MsgSwapExactAmountInResponse".to_string(),
                value: Binary::from(out.encode_to_vec()),
            }],
        }),
    };
    crate::contract::reply(deps, mock_env(), reply_msg).unwrap()
}

/// Assert `res` carries exactly one SubMsg and that it is the native-pool
/// swap-forward SubMsg (reply id 4). Returns its payload for reply-driving.
fn expect_single_swap_forward(res: &Response) -> Binary {
    assert_eq!(
        res.messages.len(),
        1,
        "SimpleSwap should emit exactly one SubMsg (the native-pool swap), got {}",
        res.messages.len()
    );
    assert_eq!(
        res.messages[0].id, REPLY_ID_SWAP_FORWARD,
        "the emitted SubMsg must be the swap-forward reply"
    );
    res.messages[0].payload.clone()
}

fn action_attr(res: &Response) -> String {
    res.attributes
        .iter()
        .find(|a| a.key == "action")
        .expect("response must carry an action attribute")
        .value
        .clone()
}

#[test]
fn test_commit_pre_threshold_basic() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token

    let env = mock_env();
    let commit_amount = Uint128::new(1_000_000_000); // 1k bluechip

    let info = message_info(
        &Addr::unchecked("user1"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let res = execute(deps.as_mut(), env.clone(), info, msg).unwrap();

    assert_eq!(res.messages.len(), 2);

    let user_addr = Addr::unchecked("user1");
    let user_commit_usd = COMMIT_LEDGER.load(&deps.storage, &user_addr).unwrap();
    assert_eq!(user_commit_usd, Uint128::new(1_000_000_000)); // $1k with 6 decimals

    let total_usd = USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
    assert_eq!(total_usd, Uint128::new(1_000_000_000));

    assert!(!IS_THRESHOLD_HIT.load(&deps.storage).unwrap());

    let committing = COMMIT_INFO.load(&deps.storage, &user_addr).unwrap();
    assert_eq!(committing.total_paid_bluechip, commit_amount);
    assert_eq!(committing.total_paid_usd, Uint128::new(1_000_000_000));
}

#[test]
fn test_race_condition_commits_crossing_threshold() {
    // Phase-2: the same-block post-threshold cooldown / swap-cap ramp is
    // gone. A follower commit landing in the SAME tx as the crossing now
    // routes into `process_post_threshold_commit`, but the native GAMM pool
    // id (`POOL_ID`) is only set by the `MsgCreateBalancerPool` reply, which
    // has NOT executed yet in this unit test. So the follower's swap leg is
    // rejected with `ShortOfThreshold` — the pool is not tradeable until the
    // create-pool reply lands. This preserves the original invariant: a
    // same-block follower cannot atomically trade against the freshly-crossed
    // pool.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(20_000_000_000),
    }]);

    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token
    THRESHOLD_PROCESSING
        .save(&mut deps.storage, &false)
        .unwrap();

    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(24_900_000_000))
        .unwrap();

    let commit_amount = Uint128::new(200_000_000); // $200 per commit
    let env = mock_env();

    let info1 = message_info(
        &Addr::unchecked("alice"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg1 = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let res1 = execute(deps.as_mut(), env.clone(), info1, msg1).unwrap();
    assert!(res1
        .attributes
        .iter()
        .any(|a| a.value == "threshold_crossing"));
    assert!(IS_THRESHOLD_HIT.load(&deps.storage).unwrap());
    assert!(!THRESHOLD_PROCESSING.load(&deps.storage).unwrap());

    let info2 = message_info(
        &Addr::unchecked("bob"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg2 = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: Some(Decimal::percent(10)),
    };

    let err2 = execute(deps.as_mut(), env.clone(), info2, msg2).unwrap_err();
    match err2 {
        ContractError::ShortOfThreshold {} => {}
        other => panic!(
            "Expected ShortOfThreshold on same-block follower commit (native pool id not yet set), got {:?}",
            other
        ),
    }

    // Bob must not have re-crossed nor been recorded in the commit ledger.
    assert!(IS_THRESHOLD_HIT.load(&deps.storage).unwrap());
    assert!(COMMIT_LEDGER
        .load(&deps.storage, &Addr::unchecked("bob"))
        .is_err());
}

#[test]
fn test_commit_crosses_threshold() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(10_000_000_000), // 10k tokens
    }]);

    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token

    THRESHOLD_PROCESSING
        .save(&mut deps.storage, &false)
        .unwrap();

    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(24_900_000_000))
        .unwrap(); // $24.9k

    let env = mock_env();
    let commit_amount = Uint128::new(200_000_000); // 200 tokens = $200

    let info = message_info(
        &Addr::unchecked("whale"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    assert!(IS_THRESHOLD_HIT.load(&deps.storage).unwrap());

    assert!(!THRESHOLD_PROCESSING.load(&deps.storage).unwrap());
    assert!(res
        .attributes
        .iter()
        .any(|attr| attr.key == "phase" && attr.value == "threshold_crossing"));

    // Phase-2: crossing emits mints + the MsgCreateBalancerPool SubMsg +
    // the factory-notify SubMsg + the post-fee excess refund. No reserves
    // are seeded locally, so the `pool_state.total_liquidity` assertion is
    // gone; the message count still holds.
    assert!(
        res.messages.len() >= 6,
        "Expected at least 6 messages, got {}",
        res.messages.len()
    );

    assert!(
        DISTRIBUTION_STATE
            .may_load(&deps.storage)
            .unwrap()
            .is_some(),
        "Distribution state should be initialized for batched payout"
    );
}

#[test]
fn test_commit_post_threshold_swap() {
    // Estimate-answering querier so the post-threshold commit's swap leg
    // derives a non-zero slippage floor (CARRY-OVER 2). `set_factory_oracle`
    // replaces `with_factory_oracle` for the PoolMockQuerier.
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000), // Give contract 1000 tokens
    }]);
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury"); // $1 per token

    let env = mock_env();
    let commit_amount = Uint128::new(100_000_000); // 100 bluechip

    let info = message_info(
        &Addr::unchecked("commiter"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        // H-3 — post-threshold commits require an explicit belief_price (the
        // only manipulation-resistant slippage bound on the swap leg).
        belief_price: Some(Decimal::one()),
        max_spread: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    // Fee messages (2) + the native-pool swap SubMsg. The swap output is
    // forwarded to the committer in the REPLY_ID_SWAP_FORWARD reply, not
    // inline, so there are no reserve/fee-growth mutations to assert.
    assert!(res.messages.len() >= 3);
    assert!(res
        .messages
        .iter()
        .any(|m| m.id == REPLY_ID_SWAP_FORWARD));
}

#[test]
fn test_threshold_payout_integrity_check() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    let mut bad_payout = THRESHOLD_PAYOUT_AMOUNTS
        .load(&deps.storage)
        .expect("failed to load payout");
    bad_payout.creator_reward_amount = Uint128::new(999_999_999_999); // Wrong total!
    THRESHOLD_PAYOUT_AMOUNTS
        .save(&mut deps.storage, &bad_payout)
        .expect("failed to save payout");

    // trigger_threshold_payout reads NATIVE_RAISED_FROM_COMMIT directly for
    // the native-pool seed; seed a value so the (pre-corruption-check) load
    // never trips.
    NATIVE_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(1_000_000))
        .unwrap();

    let pool_info = POOL_INFO.load(&deps.storage).expect("pool_info");
    let commit_config = COMMIT_LIMIT_INFO
        .load(&deps.storage)
        .expect("commit_config");
    let fee_info = COMMITFEEINFO.load(&deps.storage).expect("fee_info");
    let env = mock_env();

    let result = trigger_threshold_payout(
        &mut deps.storage,
        &cosmwasm_std::QuerierWrapper::new(&deps.querier),
        &pool_info,
        &commit_config,
        &bad_payout,
        &fee_info,
        &fee_info.bluechip_wallet_address,
        Decimal::permille(3),
        // Legacy fee context ($1/native rate, no live fee coin).
        Uint128::new(1_000_000),
        None,
        0,
        "",
        &env,
    );

    assert!(result.is_err(), "expected integrity check failure");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("corruption"),
        "unexpected error message: {}",
        err_msg
    );
}

#[test]
fn test_continue_distribution_is_permissionless() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    for i in 0..3 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }

    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000_000),
        total_committed_usd: Uint128::new(300),
        last_processed_key: None,
        distributions_remaining: 3,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: env.block.time,
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();
    let msg = ExecuteMsg::ContinueDistribution {};
    // Any external user can call ContinueDistribution — it's permissionless
    let info = message_info(&Addr::unchecked("random_user"), &[]);

    let res = execute(deps.as_mut(), mock_env(), info, msg);

    assert!(
        res.is_ok(),
        "ContinueDistribution should be permissionless, got: {:?}",
        res.unwrap_err()
    );
}

#[test]
fn test_continue_distribution_processes_batch() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    for i in 0..5 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }
    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000_000),
        total_committed_usd: Uint128::new(1_000_000_000),
        last_processed_key: None,
        distributions_remaining: 5,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        last_successful_batch_size: Some(3), // Test with previous successful batch size
        consecutive_failures: 0,
        started_at: env.block.time,
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let env = mock_env();
    // Permissionless — any user can trigger
    let info = message_info(&Addr::unchecked("anyone"), &[]);

    let msg = ExecuteMsg::ContinueDistribution {};
    let res = execute(deps.as_mut(), env, info, msg).expect("permissionless call should succeed");

    assert!(
        res.attributes
            .iter()
            .any(|a| a.value == "continue_distribution"),
        "Response should include continue_distribution attribute"
    );

    assert!(
        res.messages.len() >= 5,
        "All 5 committers should be processed in one batch with gas-based batch size"
    );
}

#[test]
fn test_continue_distribution_batches() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    for i in 0..10 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }
    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(1_000_000),
        last_processed_key: None,
        distributions_remaining: 10,
        max_gas_per_tx: 200,
        estimated_gas_per_distribution: 50,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: env.block.time,
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let env = mock_env();
    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        info,
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    // Calculate expected batch size
    let base_batch_size =
        (dist_state.max_gas_per_tx / dist_state.estimated_gas_per_distribution).max(1) as u32;
    let expected_batch_size = if dist_state.last_successful_batch_size.is_none() {
        base_batch_size.min(10).max(1) as usize
    } else {
        base_batch_size as usize
    };

    let actual_expected = expected_batch_size.min(dist_state.distributions_remaining as usize);

    // Check how many committers were actually processed
    let committers_after = COMMIT_LEDGER
        .range(&deps.storage, None, None, Order::Ascending)
        .count();
    let processed = 10 - committers_after;

    assert_eq!(
        processed, actual_expected,
        "Should process exactly {} committers based on gas limits",
        actual_expected
    );

    // Check if state was updated or removed
    match DISTRIBUTION_STATE.may_load(&deps.storage).unwrap() {
        Some(new_state) => {
            assert_eq!(
                new_state.distributions_remaining,
                dist_state.distributions_remaining - processed as u32,
                "Distributions remaining should be updated correctly"
            );

            assert_eq!(
                new_state.last_successful_batch_size,
                Some(processed as u32),
                "Should record the actual batch size that was processed"
            );

            // Messages: `processed` mint messages only. There is no
            // bounty message and no self-call ContinueDistribution —
            // external callers trigger subsequent batches in separate
            // transactions.
            assert_eq!(
                res.messages.len(),
                processed,
                "Expected exactly `processed` mint msgs, got: {:?}",
                res.messages
            );
        }
        None => {
            assert_eq!(
                processed, 10,
                "If state is removed, all 10 committers should have been processed"
            );
        }
    }
}
#[test]
fn test_adaptive_batch_sizing_with_history() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    // Add many committers
    for i in 0..20 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }
    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(1_000_000),
        last_processed_key: None,
        distributions_remaining: 20,
        max_gas_per_tx: 1000,
        estimated_gas_per_distribution: 50,
        last_successful_batch_size: Some(12),
        consecutive_failures: 0,
        started_at: env.block.time,
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let total_before = COMMIT_LEDGER
        .range(&deps.storage, None, None, Order::Ascending)
        .count();

    let env = mock_env();
    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        info,
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    // Check what's left in ledger after processing
    let total_after = COMMIT_LEDGER
        .range(&deps.storage, None, None, Order::Ascending)
        .count();
    let actually_processed = total_before - total_after;

    // Mints + 1 dust-settlement mint to the creator (the factory bounty
    // message is gone). The test inputs have `total_committed_usd =
    // 1_000_000` but the ledger sums to 2_000, so per-user
    // floor(100 * 1_000_000 / 1_000_000) = 100; 20 * 100 = 2_000 vs
    // total_to_distribute = 1_000_000, leaving a 998_000-base-unit
    // residual that the final batch settles to the creator wallet.
    assert_eq!(
        res.messages.len(),
        actually_processed + 1,
        "Expected `actually_processed` mints + 1 dust-settlement mint"
    );

    let expected = 20;
    assert_eq!(
        actually_processed, expected,
        "Should process exactly {} committers based on gas-based batch size",
        expected
    );
}

#[test]
fn test_calculate_effective_batch_size() {
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(1_000_000),
        last_processed_key: None,
        distributions_remaining: 20,
        max_gas_per_tx: 1000,
        estimated_gas_per_distribution: 50,
        last_successful_batch_size: Some(12),
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(0),
        last_updated: Timestamp::from_seconds(0),
        distributed_so_far: Uint128::zero(),
    };

    let batch_size = calculate_effective_batch_size(&dist_state);

    assert_eq!(
        batch_size, 20,
        "Should use gas-based estimate, ignoring last_successful_batch_size"
    );

    let dist_state_no_history = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(1_000_000),
        last_processed_key: None,
        distributions_remaining: 20,
        max_gas_per_tx: 1000,
        estimated_gas_per_distribution: 50,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(0),
        last_updated: Timestamp::from_seconds(0),
        distributed_so_far: Uint128::zero(),
    };

    let batch_size = calculate_effective_batch_size(&dist_state_no_history);

    assert_eq!(
        batch_size, 20,
        "Should use gas-based estimate regardless of history"
    );
}

#[test]
fn test_batch_size_with_consecutive_failures() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    for i in 0..10 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }
    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(1_000_000),
        last_processed_key: None,
        distributions_remaining: 10,
        max_gas_per_tx: 1000,
        estimated_gas_per_distribution: 200, // High estimate due to failures
        last_successful_batch_size: Some(2), // Last success was small
        consecutive_failures: 2,             // Had 2 failures
        started_at: env.block.time,          // Use current time
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let env = mock_env();
    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        info,
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    // Up to 5 mints (gas estimate cap); the pool emits no factory bounty msg.
    assert!(
        res.messages.len() <= 5,
        "Should process at most 5 committers, got {}",
        res.messages.len()
    );
}

#[test]
fn test_final_batch_completes_distribution() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    // Add exactly 3 committers
    for i in 0..3 {
        COMMIT_LEDGER
            .save(
                &mut deps.storage,
                &Addr::unchecked(format!("user{}", i)),
                &Uint128::new(100),
            )
            .unwrap();
    }
    let env = mock_env();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(1_000_000),
        total_committed_usd: Uint128::new(300),
        last_processed_key: None,
        distributions_remaining: 3,
        max_gas_per_tx: 1000,
        estimated_gas_per_distribution: 50,
        last_successful_batch_size: Some(5),
        consecutive_failures: 0,
        started_at: env.block.time, // Use current time
        last_updated: env.block.time,
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let env = mock_env();
    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        info,
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    // Should complete all remaining
    assert_eq!(
        DISTRIBUTION_STATE.may_load(&deps.storage).unwrap(),
        None,
        "Distribution state should be removed after completion"
    );

    // 3 committer mints + 1 dust-settlement mint to creator (the factory
    // bounty message is gone). With 3 committers each paying 100
    // and total_to_distribute = 1_000_000, per-user reward floors to
    // 333_333; 3 * 333_333 = 999_999, leaving 1 base unit of dust the
    // final batch settles to the creator wallet.
    assert_eq!(
        res.messages.len(),
        4,
        "Expected 3 mint messages for committers + 1 dust mint"
    );
}

#[test]
fn test_commit_reentrancy_protection() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    REENTRANCY_LOCK.save(&mut deps.storage, &true).unwrap();

    let env = mock_env();
    let info = message_info(
        &Addr::unchecked("user"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(1_000_000),
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(1_000_000),
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::ReentrancyGuard {} => (),
        _ => panic!("Expected ReentrancyGuard error"),
    }
}

#[test]
fn test_commit_rate_limiting() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000), // Give contract 1000 tokens
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token

    let mut env = mock_env();
    let user = Addr::unchecked("user");

    // $5 = MIN_COMMIT_USD_PRE_THRESHOLD; the test is about rate-limiting,
    // not commit sizing. 5 bluechip atoms @ $1/bluechip = $5 USD.
    let info = message_info(
        &user,
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(5_000_000),
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(5_000_000),
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    execute(deps.as_mut(), env.clone(), info.clone(), msg.clone()).unwrap();

    env.block.time = env.block.time.plus_seconds(30); // Only 30 seconds later

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::TooFrequentCommits { wait_time } => {
            assert_eq!(wait_time, 30);
        }
        _ => panic!("Expected TooFrequentCommits error"),
    }
}

#[test]
fn test_commit_with_deadline() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);

    let mut env = mock_env();
    env.block.time = Timestamp::from_seconds(1_000_000);

    let info = message_info(
        &Addr::unchecked("user"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(1_000_000),
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(1_000_000),
        },
        transaction_deadline: Some(Timestamp::from_seconds(999_999)),
        belief_price: None,
        max_spread: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::TransactionExpired {} => (),
        _ => panic!("Expected DeadlineExceeded error"),
    }
}

#[test]
fn test_simple_swap_bluechip_to_cw20() {
    // Uses the estimate-answering querier so the non-belief-price swap derives
    // a NON-ZERO `token_out_min_amount` (CARRY-OVER 2 rejects a zero floor).
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000); // 1k bluechip

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    // Phase-2: the swap emits ONE MsgSwapExactAmountIn SubMsg (reply id 4);
    // the output BankMsg::Send to the trader happens in the reply. No local
    // reserve/fee-growth mutation to inspect.
    assert_eq!(action_attr(&res), "swap");
    expect_single_swap_forward(&res);
}

#[test]
fn test_swap_with_max_spread() {
    // Phase-2: the only SYNCHRONOUS max-spread guard is `derive_token_out_min`
    // rejecting a `max_spread` that exceeds the hard cap (5% without
    // `allow_high_max_spread`). Realised-slippage checking moved onto the
    // native pool's `token_out_min_amount`. A `max_spread` above the cap
    // still errors at execute time.
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(10_000_000_000); // 10k bluechip (large swap)

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: Some(Decimal::percent(6)), // Above the 5% hard cap
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::MaxSpreadAssertion {} => (),
        _ => panic!("Expected MaxSpreadAssertion error"),
    }
}

#[test]
fn test_swap_sell_creator_token_native() {
    // Post-migration the creator token is a native TokenFactory denom, so
    // selling it is a plain `SimpleSwap` with the creator denom ATTACHED as
    // funds (the old CW20 `Receive`/hook path is gone). The swap emits the
    // native-pool SubMsg; the bluechip payout to the trader is produced when
    // the swap-forward reply is driven.
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[]); // estimate mock ⇒ non-zero floor (CARRY-OVER 2)
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(10_000_000_000); // 10k tokens

    // The trader attaches the creator denom directly as native funds.
    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: CREATOR_DENOM.to_string(),
            amount: swap_amount,
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::CreatorToken {
                denom: CREATOR_DENOM.to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: Some(Decimal::percent(10)),
        allow_high_max_spread: Some(true),
        to: None,
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    assert_eq!(action_attr(&res), "swap");
    let payload = expect_single_swap_forward(&res);

    // Driving the swap-forward reply pays out bluechip to the trader.
    let reply_res = drive_swap_forward_reply(deps.as_mut(), payload, 9_876);
    assert!(reply_res.messages.iter().any(|m| matches!(
        &m.msg,
        CosmosMsg::Bank(BankMsg::Send { to_address, amount })
            if to_address == "trader"
                && amount.iter().any(|c| c.denom == "ubluechip" && c.amount == Uint128::new(9_876))
    )));
}

/// TODO(phase2): the original test exercised the CW20 Receive-hook
/// anti-spoof guard (`Cw20SwapBalanceMismatch`). That entire attack surface
/// is gone — the creator token is a native TokenFactory denom now, and
/// `SimpleSwap` verifies the attached funds via `must_pay` (the bank module
/// cannot be spoofed). Repurposed to assert the native sell path REQUIRES the
/// funds to actually be attached.
#[test]
fn test_cw20_receive_rejects_balance_shortfall() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(10_000_000_000);

    // A `SimpleSwap` claiming to sell the creator token but attaching NO
    // funds is rejected by the funds check — the native analog of the old
    // spoofed-amount attack. No state is mutated.
    let no_funds = message_info(&Addr::unchecked("attacker"), &[]);
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::CreatorToken {
                denom: CREATOR_DENOM.to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: Some(Decimal::percent(5)),
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    let err = execute(deps.as_mut(), env.clone(), no_funds, msg).unwrap_err();
    // `must_pay` surfaces a no-funds / denom error; the swap is rejected
    // before any state mutation or SubMsg emission.
    let _ = err;
}

#[test]
fn test_swap_wrong_asset() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "wrong_token".to_string(),
            amount: Uint128::new(1_000_000),
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "wrong_token".to_string(),
            },
            amount: Uint128::new(1_000_000),
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::AssetMismatch {} => (),
        _ => panic!("Expected AssetMismatch error"),
    }
}

#[test]
fn test_factory_impersonation_prevented() {
    let mut deps = mock_dependencies();

    let msg = PoolInstantiateMsg {
        pool_id: 1u64,
        pool_token_info: [
            TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            TokenType::CreatorToken {
                denom: "factory/placeholder/ucreator".to_string(),
            },
        ],
        threshold_payout: None,
        used_factory_addr: Addr::unchecked("factory_contract"),
        commit_fee_info: CommitFeeInfo {
            bluechip_wallet_address: Addr::unchecked("ubluechip"),
            creator_wallet_address: Addr::unchecked("addr0000"),
            commit_fee_bluechip: Decimal::from_ratio(10u128, 100u128),
            commit_fee_creator: Decimal::from_ratio(10u128, 100u128),
        },
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        commit_threshold_limit_usd: Uint128::new(350_000_000_000),
        subdenom: "ucreator".to_string(),
        token_name: "Creator Token".to_string(),
        token_symbol: "UCREATOR".to_string(),
        token_decimals: 6,
        gamm_pool_creation_fee_amount: Uint128::zero(),
    };
    let info = message_info(&Addr::unchecked("fake_factory"), &[]); // Wrong sender!
    let err = instantiate(deps.as_mut(), mock_env(), info, msg).unwrap_err();

    match err {
        ContractError::Unauthorized {} => (),
        _ => panic!("Expected Unauthorized error"),
    }
}

#[test]
fn test_usd_tracking_consistency_across_commits() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(100_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token

    let env = mock_env();

    // Multiple commits. A commit's value toward the threshold IS its
    // gross native amount (1:1) — no oracle conversion anywhere. All
    // amounts sit above the 5_000_000 pre-threshold minimum-commit floor.
    let commits = vec![
        ("user1", 10_000_000u128),
        ("user2", 20_000_000u128),
        ("user3", 5_000_000u128),
    ];

    let mut expected_total = Uint128::zero();

    for (user, amount) in commits {
        let info = message_info(
            &Addr::unchecked(user),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: Uint128::new(amount),
            }],
        );

        let msg = ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: Uint128::new(amount),
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        };

        execute(deps.as_mut(), env.clone(), info, msg).unwrap();

        // Gross native amount counts 1:1 toward the raised total.
        let commit_value = Uint128::new(amount);
        expected_total += commit_value;

        let current_total = USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
        assert_eq!(
            current_total, expected_total,
            "raised-total tracking inconsistent after {} commit",
            user
        );
        let user_commit = COMMIT_INFO
            .load(&deps.storage, &Addr::unchecked(user))
            .unwrap();
        assert_eq!(
            user_commit.total_paid_usd, commit_value,
            "User {} commit-value tracking incorrect",
            user
        );
    }

    assert_eq!(expected_total, Uint128::new(35_000_000));
}

#[test]
fn test_usd_calculation_overflow() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(u128::MAX / 1000),
    }]);
    setup_pool_storage(&mut deps);

    let env = mock_env();
    let info = message_info(
        &Addr::unchecked("whale"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(u128::MAX / 1000),
        }],
    );

    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(u128::MAX / 1000),
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let result = execute(deps.as_mut(), env, info, msg);

    assert!(result.is_err(), "Should reject overflow");

    let err = result.unwrap_err();

    assert!(
        err.to_string().contains("Overflow")
            || err.to_string().contains("overflow")
            || err.to_string().contains("Querier system error"),
        "Error should mention overflow, got: {}",
        err
    );
}

#[test]
fn test_swap_with_belief_price_protection() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000); // 100 bluechip

    let belief_price = Some(Decimal::from_ratio(140u128, 100u128)); // 1.4

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price,
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    // A valid belief_price derives a non-erroring token_out_min floor, so
    // the swap SubMsg is emitted (the native pool enforces the floor).
    assert_eq!(action_attr(&res), "swap");
    expect_single_swap_forward(&res);
}

#[test]
fn test_swap_belief_price_rejects_bad_price_corrected() {
    // Phase-2: synchronous rejection is driven by the max_spread-vs-hard-cap
    // guard in `derive_token_out_min`. A `max_spread` above the 5% cap (no
    // `allow_high_max_spread`) errors at execute time regardless of the
    // belief price.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(10_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(10_000_000_000); // 10k bluechip

    let belief_price = Some(Decimal::from_ratio(5u128, 100u128)); // 0.05

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price,
        max_spread: Some(Decimal::percent(6)), // Above the 5% hard cap
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::MaxSpreadAssertion {} => (),
        _ => panic!("Expected MaxSpreadAssertion error, got {:?}", err),
    }
}

#[test]
fn test_belief_price_with_zero_price() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(1_000_000),
        }],
    );

    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(1_000_000),
        },
        belief_price: Some(Decimal::zero()),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::InvalidBeliefPrice {} => (),
        _ => panic!("Expected InvalidBeliefPrice error"),
    }
}

#[test]
fn test_swap_cw20_to_bluechip_direct() {
    // Native sell of the creator token: attach the creator denom as funds
    // and swap it for bluechip via `SimpleSwap`.
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[]); // estimate mock ⇒ non-zero floor (CARRY-OVER 2)
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(10_000_000_000); // 10k creator tokens

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: CREATOR_DENOM.to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::CreatorToken {
                denom: CREATOR_DENOM.to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: Some(Decimal::percent(5)),
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    assert_eq!(action_attr(&res), "swap");
    assert_eq!(
        res.attributes
            .iter()
            .find(|a| a.key == "offer_asset")
            .unwrap()
            .value,
        CREATOR_DENOM
    );

    let payload = expect_single_swap_forward(&res);

    // Driving the reply forwards bluechip to the trader.
    let reply_res = drive_swap_forward_reply(deps.as_mut(), payload, 42_000);
    assert!(reply_res.messages.iter().any(|m| matches!(
        &m.msg,
        CosmosMsg::Bank(BankMsg::Send { amount, .. })
            if amount.iter().any(|c| c.denom == "ubluechip" && c.amount == Uint128::new(42_000))
    )));
}

#[test]
fn test_swap_cw20_with_custom_recipient() {
    // Native sell routed to a custom recipient via `SimpleSwap { to }`.
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[]); // estimate mock ⇒ non-zero floor (CARRY-OVER 2)
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000);
    let recipient = MockApi::default().addr_make("beneficiary").to_string();

    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: CREATOR_DENOM.to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::CreatorToken {
                denom: CREATOR_DENOM.to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: Some(Decimal::percent(2)),
        allow_high_max_spread: None,
        to: Some(recipient.clone()),
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();

    let payload = expect_single_swap_forward(&res);

    // The custom recipient is recorded in the swap-forward payload...
    let decoded: SwapForwardPayload = from_json(&payload).unwrap();
    assert_eq!(
        decoded.receiver.to_string(),
        recipient,
        "swap-forward payload must carry the custom recipient"
    );

    // ...and the reply forwards the swapped-out bluechip to it.
    let reply_res = drive_swap_forward_reply(deps.as_mut(), payload, 12_345);
    let to_address = reply_res
        .messages
        .iter()
        .find_map(|m| {
            if let CosmosMsg::Bank(BankMsg::Send { to_address, .. }) = &m.msg {
                Some(to_address.clone())
            } else {
                None
            }
        })
        .expect("Should have bank send message");
    assert_eq!(
        to_address, recipient,
        "Bluechip should be sent to custom recipient"
    );
}

// ---------------------------------------------------------------------------
// FIX A — slippage floor = max(on-chain-estimate floor, belief-price floor)
// ---------------------------------------------------------------------------

fn token_out_min_attr(res: &Response) -> String {
    res.attributes
        .iter()
        .find(|a| a.key == "token_out_min_amount")
        .expect("swap response must carry token_out_min_amount")
        .value
        .clone()
}

/// Pure-function proof that `derive_token_out_min` returns the MORE
/// PROTECTIVE of the estimate floor and the belief floor, and preserves the
/// hard-cap + zero-belief-price rejections.
#[test]
fn test_derive_token_out_min_takes_max_of_floors() {
    use pool_core::swap::derive_token_out_min;

    // No belief price: the estimate floor is the binding (non-zero) floor.
    // 1000 * (1 - 0.005) = 995.
    let f = derive_token_out_min(Uint128::new(100), Uint128::new(1000), None, None, None).unwrap();
    assert_eq!(f, Uint128::new(995));

    // Belief floor LARGER than estimate → belief wins.
    // belief_price 0.5 → expected = 100 / 0.5 = 200 → *0.995 = 199;
    // estimate 100 → *0.995 = 99.  max(99, 199) = 199.
    let f2 = derive_token_out_min(
        Uint128::new(100),
        Uint128::new(100),
        Some(Decimal::percent(50)),
        None,
        None,
    )
    .unwrap();
    assert_eq!(f2, Uint128::new(199));

    // Estimate floor LARGER than belief → estimate wins.
    // belief_price 2.0 → expected = 100 / 2 = 50 → *0.995 = 49;
    // estimate 100 → *0.995 = 99.  max(99, 49) = 99.
    let f3 = derive_token_out_min(
        Uint128::new(100),
        Uint128::new(100),
        Some(Decimal::percent(200)),
        None,
        None,
    )
    .unwrap();
    assert_eq!(f3, Uint128::new(99));

    // Fail-soft: zero estimate + no belief → zero floor (NOT an error).
    let f4 =
        derive_token_out_min(Uint128::new(100), Uint128::zero(), None, None, None).unwrap();
    assert_eq!(f4, Uint128::zero());

    // Hard-cap rejection preserved.
    assert!(derive_token_out_min(
        Uint128::new(100),
        Uint128::new(100),
        None,
        Some(Decimal::percent(6)),
        None,
    )
    .is_err());
    // Zero-belief-price rejection preserved.
    assert!(derive_token_out_min(
        Uint128::new(100),
        Uint128::new(100),
        Some(Decimal::zero()),
        None,
        None,
    )
    .is_err());
}

/// End-to-end: a `SimpleSwap` with NO belief_price now still derives a
/// NON-ZERO `token_out_min_amount` from the on-chain poolmanager estimate
/// (closing the "floor of zero ⇒ no sandwich protection" hole). The
/// `PoolMockQuerier` answers the estimate at 1:1, default spread 0.5%.
#[test]
fn test_simple_swap_estimate_floor_sets_nonzero_token_out_min() {
    use crate::mock_querier::mock_deps_estimate;

    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000);
    // F-1 — the null-belief SimpleSwap path is now reachable only by the
    // registered router (which the mock querier reports as "registered_router").
    // The estimate floor is load-bearing on exactly that path.
    let info = message_info(
        &Addr::unchecked("registered_router"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: None, // router path → estimate floor is load-bearing
        max_spread: None,   // default 0.5%
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();
    assert_eq!(action_attr(&res), "swap");
    // estimate 100_000_000 * (1 - 0.005) = 99_500_000 — NON-ZERO.
    assert_eq!(token_out_min_attr(&res), "99500000");
    expect_single_swap_forward(&res);
}

/// Post-threshold commit swap site derives the same non-zero estimate
/// floor (FIX A applied at BOTH swap sites via the shared helper). The
/// commit's post-fee swap leg of a $100 commit (1% + 5% fees) is
/// 94_000_000 bluechip; at a 1:1 estimate and default 0.5% spread the
/// floor is 94_000_000 * 0.995 = 93_530_000.
///
/// H-3 — post-threshold commits now REQUIRE a belief_price. Here it is set
/// deliberately loose (2.0, i.e. belief_floor = 94_000_000/2 * 0.995 =
/// 46_765_000) so the on-chain ESTIMATE floor (93_530_000) is the binding
/// `max(estimate_floor, belief_floor)` term, preserving this test's intent
/// (the estimate floor is load-bearing) while satisfying the new
/// belief_price requirement.
#[test]
fn test_post_threshold_commit_estimate_floor_nonzero_token_out_min() {
    use crate::mock_querier::mock_deps_estimate;

    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");

    let env = mock_env();
    let commit_amount = Uint128::new(100_000_000); // $100 gross
    let info = message_info(
        &Addr::unchecked("commiter"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        // Loose belief_price (2.0) → belief_floor below the estimate floor,
        // so the estimate floor remains the binding term.
        belief_price: Some(Decimal::percent(200)),
        max_spread: None,
    };

    let res = execute(deps.as_mut(), env, info, msg).unwrap();
    // net-of-fees swap = 100_000_000 - 1% - 5% = 94_000_000;
    // 94_000_000 * 0.995 = 93_530_000.
    assert_eq!(token_out_min_attr(&res), "93530000");
    assert!(res.messages.iter().any(|m| m.id == REPLY_ID_SWAP_FORWARD));
}

/// H-3 — a post-threshold commit with no belief_price is rejected. The
/// on-chain estimate floor is not sandwich-resistant, so the commit swap
/// leg must carry an explicit off-chain-derived belief_price.
#[test]
fn test_post_threshold_commit_requires_belief_price() {
    use crate::mock_querier::mock_deps_estimate;

    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");

    let env = mock_env();
    let commit_amount = Uint128::new(100_000_000);
    let info = message_info(
        &Addr::unchecked("commiter"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    assert!(
        matches!(err, ContractError::BeliefPriceRequired {}),
        "post-threshold commit without belief_price must reject; got {:?}",
        err
    );
}

/// H-2 — once the pool has crossed its threshold, a commit must forward its
/// FULL 1% bluechip fee to the wallet and never top up the creation-fee
/// reserve again, even when the configured reserve target still leaves
/// "room" above what was actually retained. Pre-fix, `reserve_bluechip_fee`
/// used `CREATION_FEE_RESERVE_TARGET` as the ceiling, so a live gamm fee
/// below the target left `room > 0` and post-threshold commits kept
/// siphoning bluechip into the (now-unspendable) pool reserve.
#[test]
fn test_h2_post_threshold_commit_forwards_full_bluechip_fee() {
    use crate::mock_querier::mock_deps_estimate;
    use crate::state::{BLUECHIP_FEE_RESERVED, CREATION_FEE_RESERVE_TARGET};

    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps); // IS_THRESHOLD_HIT = true
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");

    // Simulate post-crossing state where the configured target (100 OSMO)
    // still sits ABOVE what was actually retained (10 OSMO) — i.e. room > 0.
    // Pre-fix this would have caused continued retention.
    CREATION_FEE_RESERVE_TARGET
        .save(&mut deps.storage, &Uint128::new(100_000_000))
        .unwrap();
    BLUECHIP_FEE_RESERVED
        .save(&mut deps.storage, &Uint128::new(10_000_000))
        .unwrap();

    let commit_amount = Uint128::new(100_000_000); // 1% bluechip fee = 1_000_000
    let info = message_info(
        &Addr::unchecked("commiter"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: Some(Decimal::one()),
        max_spread: None,
    };

    let res = execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    // The FULL 1% bluechip fee (1_000_000) is bank-sent to the wallet — not
    // partially retained.
    let full_fee_to_wallet = res.messages.iter().any(|m| matches!(
        &m.msg,
        cosmwasm_std::CosmosMsg::Bank(cosmwasm_std::BankMsg::Send { to_address, amount })
            if to_address == "bluechip_treasury"
                && amount.iter().any(|c| c.denom == "ubluechip" && c.amount == Uint128::new(1_000_000))
    ));
    assert!(
        full_fee_to_wallet,
        "post-threshold commit must forward the full 1% bluechip fee to the wallet; msgs: {:?}",
        res.messages
    );

    // The reserve is untouched post-threshold (no further retention).
    assert_eq!(
        BLUECHIP_FEE_RESERVED.load(&deps.storage).unwrap(),
        Uint128::new(10_000_000),
        "post-threshold commit must not grow BLUECHIP_FEE_RESERVED"
    );
}

// ---------------------------------------------------------------------------
// FIX B — COMMITTER_COUNT is O(1) and EXACT across repeat committers
// ---------------------------------------------------------------------------

#[test]
fn test_committer_count_exact_across_repeat_committers() {
    use crate::state::COMMITTER_COUNT;

    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(100_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1/token

    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 0);

    let commit = |amount: u128| ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: Uint128::new(amount),
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };

    let mut env = mock_env();

    // user1 first commit ($10) → new committer, count 1.
    let info1 = message_info(
        &Addr::unchecked("user1"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(10_000_000),
        }],
    );
    execute(deps.as_mut(), env.clone(), info1.clone(), commit(10_000_000)).unwrap();
    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 1);

    // user2 commit ($20) → new committer, count 2.
    let info2 = message_info(
        &Addr::unchecked("user2"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(20_000_000),
        }],
    );
    execute(deps.as_mut(), env.clone(), info2, commit(20_000_000)).unwrap();
    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 2);

    // user1 REPEAT commit (advance past the 60s rate-limit) → NOT new,
    // count stays 2 even though the ledger value accumulates.
    env.block.time = env.block.time.plus_seconds(61);
    execute(deps.as_mut(), env, info1, commit(10_000_000)).unwrap();
    assert_eq!(
        COMMITTER_COUNT.load(&deps.storage).unwrap(),
        2,
        "repeat committer must not double-count"
    );

    // Ground-truth cross-check: the O(1) counter equals the distinct-key
    // count of the ledger.
    let distinct = COMMIT_LEDGER
        .keys(&deps.storage, None, None, Order::Ascending)
        .count();
    assert_eq!(distinct, 2);
    assert_eq!(
        COMMIT_LEDGER
            .load(&deps.storage, &Addr::unchecked("user1"))
            .unwrap(),
        Uint128::new(20_000_000),
        "user1 ledger value accumulates across repeat commits"
    );
}

// ---------------------------------------------------------------------------
// CARRY-OVER 2 — a zero slippage floor is rejected at the swap sites.
// ---------------------------------------------------------------------------

#[test]
fn test_swap_zero_floor_rejected() {
    // A zero estimate (`set_estimate_ratio(0, 1)`) AND no belief_price ⇒ the
    // derived `token_out_min_amount` floor collapses to zero.
    // `compute_token_out_min` rejects that rather than dispatching an
    // unprotected `MsgSwapExactAmountIn`. F-1 — the null-belief path is
    // reachable only by the registered router, so this exercises it as that
    // caller (the mock querier answers RegisteredRouter = "registered_router").
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    deps.querier.set_estimate_ratio(0, 1);
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000);
    let info = message_info(
        &Addr::unchecked("registered_router"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: None,
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    let err = execute(deps.as_mut(), env, info, msg).unwrap_err();
    match err {
        ContractError::MaxSpreadAssertion {} => {}
        other => panic!("expected MaxSpreadAssertion for zero floor, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// FIX E — the 1% bluechip commit fee funds the gamm creation-fee reserve.
// ---------------------------------------------------------------------------

#[test]
fn test_fix_e_bluechip_fee_reserve_split_and_spillover() {
    // Target BELOW the per-commit bluechip fee, so ONE commit both fills the
    // reserve to the target and spills the remainder to the wallet.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1/token

    crate::state::CREATION_FEE_RESERVE_TARGET
        .save(&mut deps.storage, &Uint128::new(30_000))
        .unwrap();
    crate::state::BLUECHIP_FEE_RESERVED
        .save(&mut deps.storage, &Uint128::zero())
        .unwrap();

    let commit_amount = Uint128::new(10_000_000); // $10; 1% = 100_000, 5% = 500_000
    let info = message_info(
        &Addr::unchecked("user1"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };
    let res = execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    // reserved caps at the target (30_000); the remaining 70_000 of the
    // 100_000 bluechip fee is bank-sent to the live bluechip wallet.
    assert_eq!(
        crate::state::BLUECHIP_FEE_RESERVED
            .load(&deps.storage)
            .unwrap(),
        Uint128::new(30_000),
        "reserve fills exactly to the target"
    );
    let to_treasury: Option<Uint128> = res.messages.iter().find_map(|m| match &m.msg {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount })
            if to_address == "bluechip_treasury" =>
        {
            Some(amount[0].amount)
        }
        _ => None,
    });
    assert_eq!(
        to_treasury,
        Some(Uint128::new(70_000)),
        "spillover bluechip fee (100_000 - 30_000) is bank-sent to the wallet"
    );
    // Creator 5% fee is bank-sent immediately as before.
    let to_creator: Option<Uint128> = res.messages.iter().find_map(|m| match &m.msg {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount }) if to_address == "creator_wallet" => {
            Some(amount[0].amount)
        }
        _ => None,
    });
    assert_eq!(to_creator, Some(Uint128::new(500_000)));
}

#[test]
fn test_fix_e_bluechip_fee_fully_retained_below_target() {
    // Target ABOVE the per-commit bluechip fee: the entire 1% is retained in
    // the pool and NOTHING is bank-sent to the bluechip wallet this commit.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));

    crate::state::CREATION_FEE_RESERVE_TARGET
        .save(&mut deps.storage, &Uint128::new(1_000_000))
        .unwrap();
    crate::state::BLUECHIP_FEE_RESERVED
        .save(&mut deps.storage, &Uint128::zero())
        .unwrap();

    let commit_amount = Uint128::new(10_000_000); // 1% = 100_000 < target 1_000_000
    let info = message_info(
        &Addr::unchecked("user1"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: commit_amount,
        }],
    );
    let msg = ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };
    let res = execute(deps.as_mut(), mock_env(), info, msg).unwrap();

    assert_eq!(
        crate::state::BLUECHIP_FEE_RESERVED
            .load(&deps.storage)
            .unwrap(),
        Uint128::new(100_000),
        "the full 1% bluechip fee is retained (still below target)"
    );
    // No bluechip fee reaches the wallet this commit.
    let treasury_send = res.messages.iter().any(|m| matches!(
        &m.msg,
        CosmosMsg::Bank(BankMsg::Send { to_address, .. }) if to_address == "bluechip_treasury"
    ));
    assert!(
        !treasury_send,
        "no bluechip fee is bank-sent while the reserve is below target"
    );
}

#[test]
fn test_fix_e_crossing_seed_math_normal_and_shortfall() {
    use crate::state::{
        BLUECHIP_FEE_RESERVED, CREATION_FEE_RESERVE_TARGET, NATIVE_RAISED_FROM_COMMIT,
        SEED_LIQUIDITY,
    };

    // Run `trigger_threshold_payout` with the given reserve context on fresh
    // storage; return (seed_osmo, seed_creator, remit_is_some, reserved_after).
    fn run(
        native_raised: u128,
        reserved: u128,
        creation_fee: u128,
    ) -> (Uint128, Uint128, bool, Uint128, Uint128) {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        NATIVE_RAISED_FROM_COMMIT
            .save(&mut deps.storage, &Uint128::new(native_raised))
            .unwrap();
        BLUECHIP_FEE_RESERVED
            .save(&mut deps.storage, &Uint128::new(reserved))
            .unwrap();
        CREATION_FEE_RESERVE_TARGET
            .save(&mut deps.storage, &Uint128::new(creation_fee))
            .unwrap();

        let pool_info = POOL_INFO.load(&deps.storage).unwrap();
        let commit_config = COMMIT_LIMIT_INFO.load(&deps.storage).unwrap();
        let payout = THRESHOLD_PAYOUT_AMOUNTS.load(&deps.storage).unwrap();
        let fee_info = COMMITFEEINFO.load(&deps.storage).unwrap();
        let env = mock_env();
        let msgs = trigger_threshold_payout(
            &mut deps.storage,
            &cosmwasm_std::QuerierWrapper::new(&deps.querier),
            &pool_info,
            &commit_config,
            &payout,
            &fee_info,
            &fee_info.bluechip_wallet_address,
            Decimal::permille(3),
            // Legacy fee context: no live fee coin — exercises the
            // CREATION_FEE_RESERVE_TARGET fallback these FIX-E cases pin.
            Uint128::new(1_000_000),
            None,
            0,
            "",
            &env,
        )
        .unwrap();
        let (so, sc) = SEED_LIQUIDITY.load(&deps.storage).unwrap();
        let earmark = crate::state::CREATOR_EXCESS_POSITION
            .may_load(&deps.storage)
            .unwrap()
            .map(|p| p.bluechip_amount)
            .unwrap_or_default();
        (
            so,
            sc,
            msgs.reserve_remit.is_some(),
            BLUECHIP_FEE_RESERVED.load(&deps.storage).unwrap(),
            earmark,
        )
    }

    // Normal: reserved (50k) == creation_fee (50k) covers the gamm fee. The
    // pool holds native_raised + reserved = 5_050_000; seeding 5_000_000
    // leaves exactly the 50_000 fee. Seed unchanged; no leftover to remit.
    let (so, sc, remit, reserved_after, _) = run(5_000_000, 50_000, 50_000);
    assert_eq!(so, Uint128::new(5_000_000), "normal: seed_osmo unchanged");
    assert_eq!(sc, Uint128::new(350_000_000_000), "creator seed unchanged");
    assert!(!remit, "reserved == fee ⇒ no leftover remit");
    assert_eq!(
        reserved_after,
        Uint128::new(50_000),
        "reserve pinned at target post-crossing so post-threshold commits stop retaining"
    );

    // Shortfall: reserved (20k) < creation_fee (50k). Pool holds
    // 5_000_000 + 20_000 = 5_020_000; seeding base 5_000_000 + 50_000 fee =
    // 5_050_000 would brick, so the seed shrinks by the uncovered
    // shortfall (50k - 20k = 30k) to 4_970_000, making seed + fee ==
    // balance. Creator seed side is left as-is.
    let (so2, sc2, remit2, _, _) = run(5_000_000, 20_000, 50_000);
    assert_eq!(
        so2,
        Uint128::new(4_970_000),
        "shortfall: seed_osmo shrinks by (fee - reserved)"
    );
    assert_eq!(
        so2 + Uint128::new(50_000),
        Uint128::new(5_020_000),
        "seed_osmo + creation_fee == pool balance (no brick)"
    );
    assert_eq!(sc2, Uint128::new(350_000_000_000), "creator seed side untouched");
    assert!(!remit2);

    // Over-cap + shortfall regression: native_raised (12B) > max_lock (10B)
    // ⇒ a 2B creator earmark, AND reserved (20k) < creation_fee (50k). The
    // uncovered 30k fee shortfall must come from the SEED, never the
    // earmark — otherwise the pool would hold less OSMO than the recorded
    // earmark and the creator's later raw claim would fail. Seed = max_lock
    // - shortfall = 9_999_970_000, and the contract's residual OSMO
    // (balance - seed - fee) must equal the FULL 2B earmark.
    let (so3, _sc3, _remit3, _res3, earmark3) = run(12_000_000_000, 20_000, 50_000);
    assert_eq!(
        so3,
        Uint128::new(9_999_970_000),
        "over-cap shortfall: seed shrinks by (fee - reserved), earmark untouched"
    );
    assert_eq!(
        earmark3,
        Uint128::new(2_000_000_000),
        "earmark records the full over-raise (raised - max_lock)"
    );
    let balance = Uint128::new(12_000_000_000 + 20_000);
    assert_eq!(
        balance - so3 - Uint128::new(50_000),
        earmark3,
        "creator earmark is fully backed by the contract's post-crossing OSMO residual"
    );
}

// ---------------------------------------------------------------------------
// FIX G — native relative circuit breaker (pause below 25% of seeded).
// ---------------------------------------------------------------------------

#[test]
fn test_fix_g_breaker_pauses_below_floor() {
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);
    // Seeded per-side reference: 1_000_000 each.
    crate::state::SEED_LIQUIDITY
        .save(
            &mut deps.storage,
            &(Uint128::new(1_000_000), Uint128::new(1_000_000)),
        )
        .unwrap();
    // Live bluechip side at 200_000 (20% < 25% floor); creator side healthy.
    deps.querier.set_pool_liquidity(vec![
        Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(200_000),
        },
        Coin {
            denom: CREATOR_DENOM.to_string(),
            amount: Uint128::new(1_000_000),
        },
    ]);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000);
    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    // H-1 — a tripped breaker returns `Ok` (so the latched pause persists;
    // an `Err` would have rolled the pause writes back on-chain) and refunds
    // the attached offer coin. No swap SubMsg is dispatched.
    let res = execute(deps.as_mut(), env.clone(), info.clone(), msg.clone()).unwrap();
    assert_eq!(action_attr(&res), "swap_auto_paused_low_liquidity");
    assert!(
        !res.messages.iter().any(|m| m.id == REPLY_ID_SWAP_FORWARD),
        "no swap is dispatched when the breaker trips"
    );
    // The attached offer coin is refunded to the trader.
    let refunded = res.messages.iter().any(|m| matches!(
        &m.msg,
        cosmwasm_std::CosmosMsg::Bank(cosmwasm_std::BankMsg::Send { to_address, amount })
            if to_address == "trader"
                && amount.len() == 1
                && amount[0].denom == "ubluechip"
                && amount[0].amount == swap_amount
    ));
    assert!(refunded, "breaker refunds the attached offer coin on trip");
    assert!(
        crate::state::POOL_PAUSED.load(&deps.storage).unwrap(),
        "breaker latches POOL_PAUSED (persists because the caller returns Ok)"
    );
    assert!(
        crate::state::POOL_PAUSED_AUTO.load(&deps.storage).unwrap(),
        "breaker latches POOL_PAUSED_AUTO"
    );

    // The latch holds: a subsequent swap is now rejected at the POOL_PAUSED
    // gate, proving the pause actually stuck. Use a FRESH sender so the
    // per-address swap/commit rate limit (stamped by the first, tripped
    // call for `trader`) doesn't mask the pause rejection.
    let info2 = message_info(
        &Addr::unchecked("trader2"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );
    let err = execute(deps.as_mut(), env, info2, msg).unwrap_err();
    assert!(
        matches!(err, ContractError::PoolPausedLowLiquidity {}),
        "latched pause rejects the next swap; got {:?}",
        err
    );
}

#[test]
fn test_fix_g_breaker_allows_healthy_pool() {
    use crate::mock_querier::mock_deps_estimate;
    let mut deps = mock_deps_estimate(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_post_threshold(&mut deps);
    crate::state::SEED_LIQUIDITY
        .save(
            &mut deps.storage,
            &(Uint128::new(1_000_000), Uint128::new(1_000_000)),
        )
        .unwrap();
    // Both sides at 50% of seed (>= 25% floor) — healthy, no pause.
    deps.querier.set_pool_liquidity(vec![
        Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(500_000),
        },
        Coin {
            denom: CREATOR_DENOM.to_string(),
            amount: Uint128::new(500_000),
        },
    ]);

    let env = mock_env();
    let swap_amount = Uint128::new(100_000_000);
    let info = message_info(
        &Addr::unchecked("trader"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: swap_amount,
        }],
    );
    let msg = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        belief_price: Some(Decimal::percent(200)),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    let res = execute(deps.as_mut(), env, info, msg).unwrap();
    assert_eq!(action_attr(&res), "swap");
    expect_single_swap_forward(&res);
    assert!(
        !crate::state::POOL_PAUSED.load(&deps.storage).unwrap_or(false),
        "healthy pool is not paused by the breaker"
    );
}
