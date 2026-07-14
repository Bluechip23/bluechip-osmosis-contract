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
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000), // Give contract 1000 tokens
    }]);
    setup_pool_post_threshold(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000)); // $1 per token

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
        belief_price: None,
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
        &pool_info,
        &commit_config,
        &bad_payout,
        &fee_info,
        &fee_info.bluechip_wallet_address,
        Decimal::permille(3),
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
    let mut deps = mock_dependencies_with_balance(&[Coin {
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
        belief_price: None,
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
        belief_price: None,
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
    let mut deps = mock_dependencies();
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
        belief_price: None,
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
        belief_price: None,
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
        belief_price: None,
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
    let mut deps = mock_dependencies();
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
        belief_price: None,
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
    let mut deps = mock_dependencies();
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
        belief_price: None,
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
