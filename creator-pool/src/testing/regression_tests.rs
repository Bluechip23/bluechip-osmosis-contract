use cosmwasm_std::{
    testing::{message_info, mock_dependencies, mock_env, MockQuerier, MockStorage},
    Addr, Coin, Decimal, OwnedDeps, Timestamp, Uint128, WasmMsg,
};
use std::str::FromStr;

use crate::asset::{TokenInfo, TokenType};
use crate::contract::{execute, migrate};
use crate::error::ContractError;
use crate::msg::{ExecuteMsg, MigrateMsg};
use crate::state::{
    DistributionState, ExpectedFactory, RecoveryType, COMMIT_LEDGER,
    DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION, DEFAULT_MAX_GAS_PER_TX, DISTRIBUTION_STATE,
    EMERGENCY_DRAINED, EXPECTED_FACTORY, POOL_SPECS, REENTRANCY_LOCK, THRESHOLD_PROCESSING,
};
use crate::testing::fixtures::{
    setup_pool_post_threshold, setup_pool_storage, with_factory_oracle, CREATOR_DENOM,
};

#[test]
fn test_recover_stuck_reentrancy_guard() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    // Set up the factory address for authorization
    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Simulate stuck reentrancy guard
    REENTRANCY_LOCK.save(&mut deps.storage, &true).unwrap();
    assert!(REENTRANCY_LOCK.load(&deps.storage).unwrap());

    let env = mock_env();
    let factory_info = message_info(&Addr::unchecked("factory_contract"), &[]);

    // Recover via RecoverStuckStates
    let msg = ExecuteMsg::RecoverStuckStates {
        recovery_type: RecoveryType::StuckReentrancyGuard,
    };

    let res = execute(deps.as_mut(), env, factory_info, msg).unwrap();

    // Guard should be reset
    assert!(!REENTRANCY_LOCK.load(&deps.storage).unwrap());

    // Check response attributes
    let recovered_attr = res
        .attributes
        .iter()
        .find(|a| a.key == "recovered")
        .expect("Should have 'recovered' attribute");
    assert!(recovered_attr.value.contains("reentrancy_guard"));
}

#[test]
fn test_recover_stuck_reentrancy_guard_unauthorized() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    REENTRANCY_LOCK.save(&mut deps.storage, &true).unwrap();

    let env = mock_env();
    // Not the factory - should fail
    let hacker_info = message_info(&Addr::unchecked("hacker"), &[]);

    let msg = ExecuteMsg::RecoverStuckStates {
        recovery_type: RecoveryType::StuckReentrancyGuard,
    };

    let err = execute(deps.as_mut(), env, hacker_info, msg).unwrap_err();
    assert!(matches!(err, ContractError::Unauthorized {}));

    // Guard still stuck
    assert!(REENTRANCY_LOCK.load(&deps.storage).unwrap());
}

#[test]
fn test_recover_not_stuck_returns_error() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Guard is NOT stuck
    REENTRANCY_LOCK.save(&mut deps.storage, &false).unwrap();

    let env = mock_env();
    let factory_info = message_info(&Addr::unchecked("factory_contract"), &[]);

    let msg = ExecuteMsg::RecoverStuckStates {
        recovery_type: RecoveryType::StuckReentrancyGuard,
    };

    let err = execute(deps.as_mut(), env, factory_info, msg).unwrap_err();
    assert!(matches!(err, ContractError::NothingToRecover {}));
}

#[test]
fn test_recover_both_resets_all_stuck_states() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Simulate both stuck reentrancy guard and stuck threshold
    REENTRANCY_LOCK.save(&mut deps.storage, &true).unwrap();
    THRESHOLD_PROCESSING.save(&mut deps.storage, &true).unwrap();

    // Set last threshold attempt to far in the past so it qualifies as stuck
    use crate::state::LAST_THRESHOLD_ATTEMPT;
    LAST_THRESHOLD_ATTEMPT
        .save(&mut deps.storage, &Timestamp::from_seconds(0))
        .unwrap();

    let mut env = mock_env();
    env.block.time = Timestamp::from_seconds(7200); // 2 hours later

    let factory_info = message_info(&Addr::unchecked("factory_contract"), &[]);

    let msg = ExecuteMsg::RecoverStuckStates {
        recovery_type: RecoveryType::Both,
    };

    let res = execute(deps.as_mut(), env, factory_info, msg).unwrap();

    // Both should be reset
    assert!(!REENTRANCY_LOCK.load(&deps.storage).unwrap());
    assert!(!THRESHOLD_PROCESSING.load(&deps.storage).unwrap());

    let recovered_attr = res
        .attributes
        .iter()
        .find(|a| a.key == "recovered")
        .expect("Should have 'recovered' attribute");
    assert!(recovered_attr.value.contains("reentrancy_guard"));
    assert!(recovered_attr.value.contains("threshold"));
}

#[test]
fn test_distribution_bounty_does_not_touch_pool_funds() {
    // ContinueDistribution pays no bounty: it sends no message to the
    // factory. This test pins the invariant that ContinueDistribution
    // emits NO message to the factory at all.
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    let committer = Addr::unchecked("committer1");
    COMMIT_LEDGER
        .save(&mut deps.storage, &committer, &Uint128::new(5_000_000_000))
        .unwrap();

    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(25_000_000_000),
        last_processed_key: None,
        distributions_remaining: 1,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let env = mock_env();
    let caller_info = message_info(&Addr::unchecked("bounty_hunter"), &[]);

    let msg = ExecuteMsg::ContinueDistribution {};
    let res = execute(deps.as_mut(), env, caller_info, msg).unwrap();

    // Confirm NO message targets the factory — the bounty flow is gone.
    let factory_msg_present = res.messages.iter().any(|sm| match &sm.msg {
        cosmwasm_std::CosmosMsg::Wasm(WasmMsg::Execute { contract_addr, .. }) => {
            contract_addr == "factory_contract"
        }
        _ => false,
    });
    assert!(
        !factory_msg_present,
        "ContinueDistribution must not message the factory (bounty removed), got: {:?}",
        res.messages
    );
}

/// ContinueDistribution on an already-empty ledger must be a pure
/// cleanup no-op: no payout messages, no bounty of any kind, state
/// removed. This test sets up exactly that
/// scenario and asserts (a) the response contains zero messages, (b) the
/// `bounty_paid=false` attribute is emitted, and (c) DISTRIBUTION_STATE is
/// removed in the same tx.
#[test]
fn test_continue_distribution_skips_bounty_on_empty_batch() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Distribution is "in progress" by state, but the ledger is empty —
    // the post-final-batch window where the cursor has advanced past the
    // last entry but the state has not yet been cleaned up. The handler
    // must cope with this window rather than stall.
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(25_000_000_000),
        last_processed_key: None,
        distributions_remaining: 1,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    // No COMMIT_LEDGER entries — the empty-batch case.

    let mut env = mock_env();
    env.block.time = Timestamp::from_seconds(1_600_000_100);

    let caller = message_info(&Addr::unchecked("bounty_hunter"), &[]);
    let res = execute(
        deps.as_mut(),
        env,
        caller,
        ExecuteMsg::ContinueDistribution {},
    )
    .expect("call should still succeed — it's a clean no-op");

    // No bounty msg emitted (and no mint msgs either, since nothing to mint).
    assert!(
        res.messages.is_empty(),
        "no messages should be emitted on an empty batch, got: {:?}",
        res.messages
    );

    // Attributes should explicitly call out the no-op for observability.
    let bounty_paid = res
        .attributes
        .iter()
        .find(|a| a.key == "bounty_paid")
        .map(|a| a.value.as_str())
        .unwrap_or("");
    assert_eq!(
        bounty_paid, "false",
        "bounty_paid attribute must reflect that no bounty was emitted"
    );
    let processed = res
        .attributes
        .iter()
        .find(|a| a.key == "processed_count")
        .map(|a| a.value.as_str())
        .unwrap_or("");
    assert_eq!(processed, "0", "processed_count must reflect zero work");

    // State must be cleaned up in the same tx (ledger-emptiness termination).
    assert_eq!(
        DISTRIBUTION_STATE.may_load(&deps.storage).unwrap(),
        None,
        "DISTRIBUTION_STATE must be removed when the ledger is empty"
    );
}

/// Regression: when the batch processes the FINAL committer, the bounty IS
/// paid AND the state is removed in the same tx — no extra empty cleanup
/// call required. Pins that the natural-completion path doesn't regress.
#[test]
fn test_continue_distribution_completes_in_one_tx_when_final() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Single committer — one mint msg, then state removed.
    let committer = Addr::unchecked("only_committer");
    COMMIT_LEDGER
        .save(&mut deps.storage, &committer, &Uint128::new(5_000_000_000))
        .unwrap();

    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(5_000_000_000),
        last_processed_key: None,
        distributions_remaining: 1,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let mut env = mock_env();
    env.block.time = Timestamp::from_seconds(1_600_000_100);

    let caller = message_info(&Addr::unchecked("bounty_hunter"), &[]);
    let res = execute(
        deps.as_mut(),
        env,
        caller,
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    assert_eq!(
        res.messages.len(),
        1,
        "expected exactly 1 mint msg (the pool emits no bounty msg), got: {:?}",
        res.messages
    );
    let bounty_paid = res
        .attributes
        .iter()
        .find(|a| a.key == "bounty_paid")
        .map(|a| a.value.as_str())
        .unwrap_or("");
    assert_eq!(bounty_paid, "true");

    let complete = res
        .attributes
        .iter()
        .find(|a| a.key == "distribution_complete")
        .map(|a| a.value.as_str())
        .unwrap_or("");
    assert_eq!(complete, "true", "should complete in this single tx");

    assert_eq!(
        DISTRIBUTION_STATE.may_load(&deps.storage).unwrap(),
        None,
        "DISTRIBUTION_STATE must be removed when the ledger is fully drained"
    );
    // Ledger is empty.
    assert_eq!(
        COMMIT_LEDGER
            .keys(&deps.storage, None, None, cosmwasm_std::Order::Ascending)
            .count(),
        0
    );
}

#[test]
fn test_migrate_rejects_excessive_fees() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();

    // Try to set fee to 11% (above 10% cap) - should fail
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::percent(11),
    };

    let err = migrate(deps.as_mut(), env.clone(), msg).unwrap_err();
    assert!(
        matches!(err, ContractError::LpFeeOutOfRange { .. }),
        "fees above 10% should be rejected, got: {}",
        err
    );
}

#[test]
fn test_migrate_accepts_valid_fees() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();

    // Set fee to exactly 10% (boundary) - should succeed
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::percent(10),
    };

    let res = migrate(deps.as_mut(), env.clone(), msg).unwrap();
    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "action" && a.value == "migrate"));

    // Verify the fee was actually updated
    let pool_specs = POOL_SPECS.load(&deps.storage).unwrap();
    assert_eq!(pool_specs.lp_fee, Decimal::percent(10));
}

#[test]
fn test_migrate_accepts_small_fees() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();

    // Set fee to 0.3% - typical AMM fee
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::from_str("0.003").unwrap(),
    };

    let _res = migrate(deps.as_mut(), env, msg).unwrap();
    let pool_specs = POOL_SPECS.load(&deps.storage).unwrap();
    assert_eq!(pool_specs.lp_fee, Decimal::from_str("0.003").unwrap());
}

// ==================== Additional Regression Tests ====================

/// Verify migrate rejects fees below 0.1% minimum
#[test]
fn test_migrate_rejects_zero_fees() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::zero(),
    };

    let err = migrate(deps.as_mut(), env, msg).unwrap_err();
    assert!(
        matches!(err, ContractError::LpFeeOutOfRange { .. }),
        "zero fees should be rejected, got: {}",
        err
    );
}

/// Verify migrate rejects fees just below the 0.1% minimum
#[test]
fn test_migrate_rejects_below_minimum() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::from_str("0.0009").unwrap(), // 0.09% < 0.1%
    };

    let err = migrate(deps.as_mut(), env, msg).unwrap_err();
    assert!(
        matches!(err, ContractError::LpFeeOutOfRange { .. }),
        "fees below 0.1% should be rejected, got: {}",
        err
    );
}

/// Verify migrate accepts fees at exactly 0.1% minimum
#[test]
fn test_migrate_accepts_minimum_fee() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    let env = mock_env();
    let msg = MigrateMsg::UpdateFees {
        new_fees: Decimal::permille(1), // 0.1%
    };

    let _res = migrate(deps.as_mut(), env, msg).unwrap();
    let pool_specs = POOL_SPECS.load(&deps.storage).unwrap();
    assert_eq!(pool_specs.lp_fee, Decimal::permille(1));
}

/// Verify emergency withdrawal clears distribution state
#[test]
fn test_emergency_withdraw_clears_distribution() {
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Set up an in-progress distribution
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(25_000_000_000),
        last_processed_key: None,
        distributions_remaining: 50,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    // `execute_emergency_withdraw_initiate` reads the admin-tunable
    // delay at runtime via `query_wasm_smart`, so the synchronous
    // wasm-querier must mock the factory's response. The pool's
    // configured factory_addr from `setup_pool_post_threshold` is
    // `"factory_contract"`.
    deps.querier.update_wasm(move |query| match query {
        cosmwasm_std::WasmQuery::Smart { contract_addr, .. }
            if contract_addr == "factory_contract" =>
        {
            let resp = pool_factory_interfaces::EmergencyWithdrawDelayResponse {
                delay_seconds: 86_400,
            };
            cosmwasm_std::SystemResult::Ok(cosmwasm_std::ContractResult::Ok(
                cosmwasm_std::to_json_binary(&resp).unwrap(),
            ))
        }
        _ => cosmwasm_std::SystemResult::Err(cosmwasm_std::SystemError::InvalidRequest {
            error: "unmocked wasm query".to_string(),
            request: cosmwasm_std::Binary::default(),
        }),
    });

    // Phase 1: initiate emergency withdrawal
    let mut env = mock_env();
    env.block.time = Timestamp::from_seconds(1_700_000_000);
    let factory_info = message_info(&Addr::unchecked("factory_contract"), &[]);
    execute(
        deps.as_mut(),
        env.clone(),
        factory_info.clone(),
        ExecuteMsg::EmergencyWithdraw {},
    )
    .unwrap();

    // Phase 2: execute after timelock (24h + 1s)
    env.block.time = Timestamp::from_seconds(1_700_000_000 + 86_401);
    execute(
        deps.as_mut(),
        env,
        factory_info,
        ExecuteMsg::EmergencyWithdraw {},
    )
    .unwrap();

    // Distribution should be cleared
    let post_dist = DISTRIBUTION_STATE.load(&deps.storage).unwrap();
    assert!(
        !post_dist.is_distributing,
        "distribution should be stopped after emergency withdrawal"
    );
    assert_eq!(
        post_dist.distributions_remaining, 0,
        "distributions_remaining should be 0 after emergency withdrawal"
    );

    // Pool should be permanently drained
    assert!(EMERGENCY_DRAINED.load(&deps.storage).unwrap());
}

// ---------------------------------------------------------------------------
// `Commit` must reject multi-denom funds via `must_pay`. Without that
// gate, attaching `[ubluechip: amount, ibc/...: Y]` would let the
// bluechip-side equality check pass while the IBC side was silently
// absorbed into the pool's bank balance with no withdrawal path. This
// test asserts a commit with extra denoms is rejected.
// ---------------------------------------------------------------------------
#[test]
fn test_commit_rejects_multi_denom_funds() {
    use crate::msg::CommitFeeInfo;
    use crate::state::CommitLimitInfo;
    use crate::state::{COMMITFEEINFO, COMMIT_LIMIT_INFO, IS_THRESHOLD_HIT};

    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));

    COMMITFEEINFO
        .save(
            &mut deps.storage,
            &CommitFeeInfo {
                bluechip_wallet_address: Addr::unchecked("bluechip_wallet"),
                creator_wallet_address: Addr::unchecked("creator_wallet"),
                commit_fee_bluechip: Decimal::percent(1),
                commit_fee_creator: Decimal::percent(5),
            },
        )
        .unwrap();
    COMMIT_LIMIT_INFO
        .save(
            &mut deps.storage,
            &CommitLimitInfo {
                commit_amount_for_threshold_usd: Uint128::new(25_000_000_000),
                max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
                creator_excess_liquidity_lock_days: 14,
                min_commit_usd_pre_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_PRE_THRESHOLD,
                min_commit_usd_post_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_POST_THRESHOLD,
            },
        )
        .unwrap();
    IS_THRESHOLD_HIT.save(&mut deps.storage, &false).unwrap();

    // No oracle mock needed: a commit's value toward the threshold IS
    // its gross native amount — the commit flow makes no cross-contract
    // price query before the funds-validation gate fires.

    let env = mock_env();
    let user = Addr::unchecked("committer");
    let amount = Uint128::new(100_000_000);

    // Attaching ubluechip + a stray IBC denom must reject — otherwise
    // this call would silently absorb the IBC funds into the pool.
    let result = execute(
        deps.as_mut(),
        env,
        message_info(
            &user,
            &[
                Coin::new(amount.u128(), "ubluechip"),
                Coin::new(42_000_000u128, "ibc/27394FB...ATOM"),
            ],
        ),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    );

    let err = result.expect_err("multi-denom commit must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("Invalid commit funds")
            || msg.contains("must_pay")
            || msg.contains("additional denoms")
            || msg.contains("Sent more than one denomination")
            || msg.contains("Multiple denominations")
            || msg.contains("multiple"),
        "expected multi-denom rejection error, got: {}",
        msg
    );
}

#[test]
fn test_admin_pause_overrides_auto_flag() {
    use crate::admin::execute_pause;
    use crate::state::{POOL_INFO, POOL_PAUSED, POOL_PAUSED_AUTO};
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    // Pre-arm an auto-pause (simulating a prior remove that drained).
    POOL_PAUSED.save(&mut deps.storage, &true).unwrap();
    POOL_PAUSED_AUTO.save(&mut deps.storage, &true).unwrap();

    // Admin then issues an explicit Pause. The auto-flag must clear so
    // a later deposit (which would auto-unpause auto-state) can't
    // override the admin's intent.
    let pool_info = POOL_INFO.load(&deps.storage).unwrap();
    let factory_info = message_info(&pool_info.factory_addr, &[]);
    execute_pause(deps.as_mut(), mock_env(), factory_info).unwrap();

    assert!(POOL_PAUSED.load(&deps.storage).unwrap());
    assert!(!POOL_PAUSED_AUTO.load(&deps.storage).unwrap());
}

// ---------------------------------------------------------------------------
// Migrate must reject downgrades. With cw2 stored at version "9.9.9"
// (a far-future version that exceeds the current CARGO_PKG_VERSION),
// migrate must error rather than silently overwrite.
// ---------------------------------------------------------------------------
#[test]
fn test_migrate_rejects_downgrade() {
    use crate::contract::migrate;
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    // Force a "stored" semver that exceeds anything realistic the
    // current binary could be.
    cw2::set_contract_version(&mut deps.storage, "bluechip-osmosis-creator-pool", "9.9.9").unwrap();

    let res = migrate(
        deps.as_mut(),
        mock_env(),
        crate::msg::MigrateMsg::UpdateVersion {},
    );
    let err = res.expect_err("downgrade migration must be rejected");
    assert!(
        err.to_string().contains("downgrade"),
        "expected downgrade-rejection error, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Per-address rate limit on ContinueDistribution. A second call from
// the same address within the cooldown window must reject.
// ---------------------------------------------------------------------------
#[test]
fn test_continue_distribution_rate_limit_per_address() {
    use crate::msg::ExecuteMsg;
    use crate::state::{
        DistributionState, COMMIT_LEDGER, DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        DEFAULT_MAX_GAS_PER_TX, DISTRIBUTION_STATE, EXPECTED_FACTORY,
    };
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &crate::state::ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Seed a non-empty ledger so the first call processes work and
    // emits a bounty msg (otherwise the no-op early-return path would
    // not stamp the rate-limit timestamp the same way — actually it
    // does, but seeding makes the test exercise the productive branch).
    COMMIT_LEDGER
        .save(
            &mut deps.storage,
            &Addr::unchecked("committer1"),
            &Uint128::new(5_000_000_000),
        )
        .unwrap();
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(25_000_000_000),
        last_processed_key: None,
        distributions_remaining: 1,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let keeper = Addr::unchecked("keeper1");
    let env = mock_env();

    // First call from keeper1: succeeds.
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&keeper, &[]),
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();

    // Restock ledger so the second call has work to do (otherwise it
    // would return Err("NothingToRecover") before reaching rate-limit
    // gate — we're testing rate-limit, not the empty-ledger reject).
    COMMIT_LEDGER
        .save(
            &mut deps.storage,
            &Addr::unchecked("committer2"),
            &Uint128::new(5_000_000_000),
        )
        .unwrap();

    // Second call from same keeper, same block: must rate-limit reject.
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(&keeper, &[]),
        ExecuteMsg::ContinueDistribution {},
    );
    let err = res.expect_err("rapid second call must be rate-limited");
    assert!(
        err.to_string().contains("Rate-limited"),
        "expected rate-limit error, got: {}",
        err
    );

    // Different keeper in same block: NOT rate-limited (per-address).
    // Need to also restore DISTRIBUTION_STATE because the first call
    // emptied the original ledger and removed the state. Re-seed both.
    let dist_state = DistributionState {
        is_distributing: true,
        total_to_distribute: Uint128::new(500_000_000_000),
        total_committed_usd: Uint128::new(25_000_000_000),
        last_processed_key: None,
        distributions_remaining: 1,
        estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
        max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
        last_successful_batch_size: None,
        consecutive_failures: 0,
        started_at: Timestamp::from_seconds(1_600_000_000),
        last_updated: Timestamp::from_seconds(1_600_000_000),
        distributed_so_far: Uint128::zero(),
    };
    DISTRIBUTION_STATE
        .save(&mut deps.storage, &dist_state)
        .unwrap();

    let keeper2 = Addr::unchecked("keeper2");
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&keeper2, &[]),
        ExecuteMsg::ContinueDistribution {},
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// `RecoverStuckStates` must reject when pool is drained. The recovery
// branches don't produce fund-flow on a drained pool but they would
// leave misleading DISTRIBUTION_STATE. Failing here keeps post-drain
// state queries honest.
// ---------------------------------------------------------------------------
#[test]
fn test_recover_rejects_on_drained_pool() {
    use crate::msg::ExecuteMsg;
    use crate::state::{
        EmergencyWithdrawalInfo, ExpectedFactory, RecoveryType, EMERGENCY_DRAINED,
        EMERGENCY_WITHDRAWAL, EXPECTED_FACTORY,
    };
    let mut deps = mock_dependencies();
    setup_pool_post_threshold(&mut deps);

    EXPECTED_FACTORY
        .save(
            &mut deps.storage,
            &ExpectedFactory {
                expected_factory_address: Addr::unchecked("factory_contract"),
            },
        )
        .unwrap();

    // Mark the pool as drained.
    EMERGENCY_DRAINED.save(&mut deps.storage, &true).unwrap();
    EMERGENCY_WITHDRAWAL
        .save(
            &mut deps.storage,
            &EmergencyWithdrawalInfo {
                withdrawn_at: 1_600_000_000,
                recipient: Addr::unchecked("bluechip_wallet"),
                amount0: Uint128::new(1_000_000),
                amount1: Uint128::new(1_000_000),
                total_liquidity_at_withdrawal: Uint128::new(1_000),
            },
        )
        .unwrap();

    let factory_info = message_info(&Addr::unchecked("factory_contract"), &[]);
    let res = execute(
        deps.as_mut(),
        mock_env(),
        factory_info,
        ExecuteMsg::RecoverStuckStates {
            recovery_type: RecoveryType::Both,
        },
    );
    let err = res.expect_err("recovery on drained pool must reject");
    assert!(
        matches!(err, ContractError::EmergencyDrained {}),
        "expected EmergencyDrained, got: {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// distribution liveness primitives
// ---------------------------------------------------------------------------
//
// Coverage for the distribution liveness primitives (per-mint
// reply isolation, self-recover, claim entry):
//
// - Per-mint isolation: a single failing recipient lands in
// `FAILED_MINTS` rather than reverting the whole batch tx; the
// other rows in the batch still mint, the cursor advances.
// - SelfRecoverDistribution: permissionless after the 7-day
// `PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS` window; rejected
// before the window, accepted after.
// - ClaimFailedDistribution: committer (or anyone with their key)
// pulls an earlier failed mint out of FAILED_MINTS, optionally
// redirected to a fresh wallet. Re-failures recurse cleanly back
// into FAILED_MINTS via the same reply-isolation harness.
mod distribution_liveness_tests {
    use super::*;
    use crate::contract::reply;
    use crate::state::{
        ExpectedFactory, PendingMint, FAILED_MINTS, PENDING_MINT_REPLIES,
        PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS, REPLY_ID_DISTRIBUTION_MINT_BASE,
        STUCK_DISTRIBUTION_RECOVERY_WINDOW_SECONDS,
    };
    use cosmwasm_std::testing::MockApi;
    use cosmwasm_std::{Binary, Reply, SubMsgResponse, SubMsgResult, Timestamp};

    /// Bech32-valid address from a human-readable label. Production
    /// passes addresses that have always come through `addr_validate`
    /// (info.sender + storage round-trips). The handlers we're testing
    /// call `addr_validate` on String params, so test inputs that
    /// reach them must be bech32-valid — `Addr::unchecked("label")`
    /// is not. `MockApi::default().addr_make(...)` produces a stable
    /// bech32 address derived from the label.
    fn label_addr(label: &str) -> Addr {
        MockApi::default().addr_make(label)
    }

    fn factory_addr() -> Addr {
        // EXPECTED_FACTORY's auth check compares `info.sender` to a
        // stored Addr by equality, so any consistent value works as
        // long as the test installs the same address into both. We
        // keep `Addr::unchecked` here for symmetry with the existing
        // `check_correct_factory` helper in threshold_tests.
        Addr::unchecked("factory_address")
    }

    fn install_factory(deps: &mut OwnedDeps<MockStorage, MockApi, MockQuerier>) {
        EXPECTED_FACTORY
            .save(
                &mut deps.storage,
                &ExpectedFactory {
                    expected_factory_address: factory_addr(),
                },
            )
            .unwrap();
        // Post-instantiate admin gates read from POOL_INFO.factory_addr
        // rather than EXPECTED_FACTORY (one source of truth — see the
        // doc-comment on `pool_core::state::ExpectedFactory`). Test
        // fixtures that override EXPECTED_FACTORY must
        // update POOL_INFO too so the auth path sees the test's
        // chosen factory address.
        use pool_core::state::POOL_INFO;
        let mut pool_info = POOL_INFO.load(&deps.storage).unwrap();
        pool_info.factory_addr = factory_addr();
        POOL_INFO.save(&mut deps.storage, &pool_info).unwrap();
    }

    fn synthetic_reply(id: u64, ok: bool, err_msg: Option<&str>) -> Reply {
        #[allow(deprecated)]
        let ok_response = SubMsgResponse {
            events: vec![],
            data: None,
            msg_responses: vec![],
        };
        Reply {
            id,
            payload: Binary::default(),
            gas_used: 0,
            result: if ok {
                SubMsgResult::Ok(ok_response)
            } else {
                SubMsgResult::Err(
                    err_msg
                        .unwrap_or("CW20 mint rejected by recipient")
                        .to_string(),
                )
            },
        }
    }

    /// Per-mint isolation: when `process_distribution_batch` dispatches
    /// a per-user mint as a `reply_always` SubMsg and the mint fails,
    /// the contract's reply handler must
    /// (a) NOT propagate the error,
    /// (b) clear the PENDING_MINT_REPLIES stash for that id,
    /// (c) accumulate the failed amount under the user in FAILED_MINTS,
    /// (d) emit `distribution_mint_isolated_failure` action.
    /// This is the load-bearing liveness invariant — without it, a
    /// single rejecting recipient reverts the batch tx and stalls
    /// distribution for every committer.
    #[test]
    fn reply_distribution_mint_failure_is_isolated_into_failed_mints() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let user = Addr::unchecked("poison_committer");
        let amount = Uint128::new(123_456);
        let reply_id = REPLY_ID_DISTRIBUTION_MINT_BASE + 7;

        PENDING_MINT_REPLIES
            .save(
                &mut deps.storage,
                reply_id,
                &PendingMint {
                    user: user.clone(),
                    amount,
                },
            )
            .unwrap();

        // Reply handler must NOT propagate the error; it's the whole
        // point of the isolation.
        let r = synthetic_reply(reply_id, false, Some("recipient blacklisted"));
        let res = reply(deps.as_mut(), mock_env(), r)
            .expect("reply must Ok on Err result; isolation invariant");

        // Stash cleared.
        assert!(PENDING_MINT_REPLIES
            .may_load(&deps.storage, reply_id)
            .unwrap()
            .is_none());

        // FAILED_MINTS now holds the owed amount under the user.
        let owed = FAILED_MINTS.load(&deps.storage, &user).unwrap();
        assert_eq!(owed, amount);

        // Action attribute identifies the isolated-failure path so
        // off-chain monitoring can flag it.
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "distribution_mint_isolated_failure"));
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "user" && a.value == user.to_string()));
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "reason" && a.value.contains("blacklisted")));
    }

    /// Reply Ok branch: stash cleared, NO FAILED_MINTS write, success
    /// attribute emitted. Pre-existing entries for the user are preserved
    /// (they belong to PRIOR failed mints, not this one).
    #[test]
    fn reply_distribution_mint_success_clears_stash_only() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let user = Addr::unchecked("happy_committer");
        let reply_id = REPLY_ID_DISTRIBUTION_MINT_BASE + 99;
        // Pre-existing FAILED_MINTS entry — must be untouched on success.
        FAILED_MINTS
            .save(&mut deps.storage, &user, &Uint128::new(1_000))
            .unwrap();

        PENDING_MINT_REPLIES
            .save(
                &mut deps.storage,
                reply_id,
                &PendingMint {
                    user: user.clone(),
                    amount: Uint128::new(50),
                },
            )
            .unwrap();

        let r = synthetic_reply(reply_id, true, None);
        let res = reply(deps.as_mut(), mock_env(), r).expect("ok branch");

        assert!(PENDING_MINT_REPLIES
            .may_load(&deps.storage, reply_id)
            .unwrap()
            .is_none());
        // Pre-existing entry preserved.
        assert_eq!(
            FAILED_MINTS.load(&deps.storage, &user).unwrap(),
            Uint128::new(1_000)
        );
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "distribution_mint_succeeded"));
    }

    /// Multiple isolated failures across batches accumulate per-user.
    /// Without the `checked_add` accumulator, a second failure would
    /// overwrite the first. Verify saturation-safe addition.
    #[test]
    fn reply_distribution_mint_failures_accumulate_per_user() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let user = Addr::unchecked("repeat_failure");
        let id1 = REPLY_ID_DISTRIBUTION_MINT_BASE + 100;
        let id2 = REPLY_ID_DISTRIBUTION_MINT_BASE + 101;

        for (id, amt) in [(id1, 250u128), (id2, 750u128)] {
            PENDING_MINT_REPLIES
                .save(
                    &mut deps.storage,
                    id,
                    &PendingMint {
                        user: user.clone(),
                        amount: Uint128::new(amt),
                    },
                )
                .unwrap();
            let r = synthetic_reply(id, false, None);
            reply(deps.as_mut(), mock_env(), r).unwrap();
        }

        assert_eq!(
            FAILED_MINTS.load(&deps.storage, &user).unwrap(),
            Uint128::new(1_000),
            "two failures must accumulate, not overwrite"
        );
    }

    /// Reply id ≥ BASE but with no PENDING_MINT_REPLIES stash falls
    /// through to the canonical "unknown reply id" handler — matching
    /// the invariant pinned by `reply_unknown_id_returns_error`
    /// (which uses 0xDEADBEEF, in this range).
    #[test]
    fn reply_in_distribution_range_without_stash_is_unknown() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let r = synthetic_reply(REPLY_ID_DISTRIBUTION_MINT_BASE + 12_345, true, None);
        let err = reply(deps.as_mut(), mock_env(), r).unwrap_err();
        assert!(
            err.to_string().contains("unknown reply id"),
            "fallthrough must produce unknown-id error, got: {}",
            err
        );
    }

    // There is no per-user "skip" primitive: the scenario it would
    // target (a corrupt ledger row that `range(..)` cannot
    // deserialize) is practically unreachable with `cw_storage_plus`
    // static typing. Per-mint reply isolation
    // (FAILED_MINTS / ClaimFailedDistribution) handles every
    // realistic "one recipient can't be minted to" case automatically.

    // ----- SelfRecoverDistribution ------------------------------------

    /// Below the 7-day window, self-recover must reject so the admin's
    /// shorter (1h) recovery path has uncontested priority.
    #[test]
    fn self_recover_before_window_is_rejected() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let started = mock_env().block.time;
        let dist = DistributionState {
            is_distributing: true,
            total_to_distribute: Uint128::new(1),
            total_committed_usd: Uint128::new(1),
            last_processed_key: None,
            distributions_remaining: 1,
            estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
            max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
            last_successful_batch_size: None,
            consecutive_failures: 0,
            started_at: started,
            last_updated: started,
            distributed_so_far: Uint128::zero(),
        };
        DISTRIBUTION_STATE.save(&mut deps.storage, &dist).unwrap();

        // Just under the window.
        let mut env = mock_env();
        env.block.time = started.plus_seconds(PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS - 1);

        let info = message_info(&Addr::unchecked("any_caller"), &[]);
        let err = execute(
            deps.as_mut(),
            env,
            info,
            ExecuteMsg::SelfRecoverDistribution {},
        )
        .unwrap_err();
        match err {
            ContractError::DistributionNotStalledForSelfRecover {
                window,
                admin_window,
                ..
            } => {
                assert_eq!(window, PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS);
                assert_eq!(admin_window, STUCK_DISTRIBUTION_RECOVERY_WINDOW_SECONDS);
            }
            other => panic!(
                "expected DistributionNotStalledForSelfRecover, got: {:?}",
                other
            ),
        }
    }

    /// After the 7-day window, ANY caller can restart distribution.
    /// Cursor reset to None, counters cleared, `distributed_so_far`
    /// preserved for the dust-settlement invariant.
    #[test]
    fn self_recover_after_window_restarts_with_preserved_distributed_so_far() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let started = mock_env().block.time;
        let preserved = Uint128::new(777_777_777);
        let dist = DistributionState {
            is_distributing: false, // pretend it stalled
            total_to_distribute: Uint128::new(1_000_000_000),
            total_committed_usd: Uint128::new(10_000_000_000),
            last_processed_key: Some(Addr::unchecked("checkpoint")),
            distributions_remaining: 7,
            estimated_gas_per_distribution: 999,
            max_gas_per_tx: 999_999,
            last_successful_batch_size: Some(3),
            consecutive_failures: 5,
            started_at: started,
            last_updated: started,
            distributed_so_far: preserved,
        };
        DISTRIBUTION_STATE.save(&mut deps.storage, &dist).unwrap();

        // Seed two committers so the recovery path lands in the
        // "remaining > 0 → restart" branch.
        for label in ["committer_a", "committer_b"] {
            COMMIT_LEDGER
                .save(
                    &mut deps.storage,
                    &Addr::unchecked(label),
                    &Uint128::new(1_000),
                )
                .unwrap();
        }

        let mut env = mock_env();
        env.block.time = started.plus_seconds(PUBLIC_DISTRIBUTION_RECOVERY_WINDOW_SECONDS + 1);

        let info = message_info(&Addr::unchecked("public_keeper"), &[]);
        let res = execute(
            deps.as_mut(),
            env.clone(),
            info,
            ExecuteMsg::SelfRecoverDistribution {},
        )
        .expect("post-window must succeed");

        let dist_after = DISTRIBUTION_STATE.load(&deps.storage).unwrap();
        assert!(dist_after.is_distributing);
        assert!(
            dist_after.last_processed_key.is_none(),
            "cursor must be reset"
        );
        assert_eq!(dist_after.consecutive_failures, 0);
        assert_eq!(
            dist_after.distributed_so_far, preserved,
            "distributed_so_far must be preserved across restart so dust settlement stays correct"
        );
        assert_eq!(dist_after.distributions_remaining, 2);
        assert_eq!(dist_after.last_updated, env.block.time);

        // Observability: action attribute and stall_elapsed_seconds attr.
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "self_recover_distribution"));
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "remaining_committers" && a.value == "2"));
    }

    /// Self-recover with no DISTRIBUTION_STATE returns the dedicated
    /// error so callers don't rely on a generic "not found" shape.
    #[test]
    fn self_recover_no_distribution_returns_dedicated_error() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let info = message_info(&Addr::unchecked("nobody"), &[]);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::SelfRecoverDistribution {},
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::NoDistributionToSelfRecover));
    }

    // ----- ClaimFailedDistribution ------------------------------------

    /// Claim auth: caller must have a non-zero FAILED_MINTS entry.
    #[test]
    fn claim_failed_distribution_no_entry_rejected() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let info = message_info(&Addr::unchecked("not_a_committer"), &[]);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::ClaimFailedDistribution { recipient: None },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::NoFailedMintEntry { .. }));
    }

    /// Happy path: caller has a FAILED_MINTS entry; handler dispatches
    /// a SubMsg::reply_always for the mint, removes the FAILED_MINTS
    /// entry up front, and stashes a PENDING_MINT entry for the new
    /// reply id. On reply success the stash clears. On reply failure
    /// the amount is re-credited under the original committer for
    /// another retry.
    #[test]
    fn claim_failed_distribution_dispatches_isolated_submsg() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let user = Addr::unchecked("recovered_committer");
        let owed = Uint128::new(444_444);
        FAILED_MINTS.save(&mut deps.storage, &user, &owed).unwrap();

        // Caller specifies an alternate recipient (e.g., a fresh wallet
        // because their original is the reason the mint failed).
        // Bech32-valid because the handler addr_validates the param.
        let alternate = label_addr("fresh_wallet");
        let info = message_info(&user, &[]);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::ClaimFailedDistribution {
                recipient: Some(alternate.to_string()),
            },
        )
        .expect("claim must succeed");

        // FAILED_MINTS entry removed up front.
        assert!(FAILED_MINTS
            .may_load(&deps.storage, &user)
            .unwrap()
            .is_none());

        // Exactly one SubMsg dispatched, in the reply_always range.
        assert_eq!(res.messages.len(), 1);
        let sub = &res.messages[0];
        assert!(sub.id >= REPLY_ID_DISTRIBUTION_MINT_BASE);

        // PENDING_MINT_REPLIES recorded the user as the canonical
        // accounting key (NOT the alternate recipient) so a re-failure
        // re-credits the original committer.
        let pending = PENDING_MINT_REPLIES.load(&deps.storage, sub.id).unwrap();
        assert_eq!(pending.user, user);
        assert_eq!(pending.amount, owed);

        // The mint is now a TokenFactory MsgMint (CosmosMsg::Any) minted
        // by the pool (denom admin) to the alternate recipient — was a
        // Cw20ExecuteMsg::Mint pre-migration. Compare against the exact
        // builder output.
        let expected = pool_core::osmosis_msgs::mint_msg(
            &Addr::unchecked("pool_contract"),
            CREATOR_DENOM,
            owed,
            &alternate,
        );
        assert_eq!(
            sub.msg, expected,
            "expected a TokenFactory MsgMint to the alternate recipient, got: {:?}",
            sub.msg
        );
    }

    /// Re-failure recursion: the alternate recipient is ALSO blocked.
    /// The reply handler must re-credit the ORIGINAL committer's
    /// FAILED_MINTS entry so they can try yet another recipient. This
    /// is the loop-closure invariant — without it, the second failure
    /// would orphan the funds.
    #[test]
    fn claim_failed_distribution_re_failure_re_credits_original_committer() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let user = Addr::unchecked("loop_committer");
        let owed = Uint128::new(99_999);
        FAILED_MINTS.save(&mut deps.storage, &user, &owed).unwrap();

        // Bech32 needed for addr_validate.
        let alternate = label_addr("alternate_also_blocked");
        let info = message_info(&user, &[]);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::ClaimFailedDistribution {
                recipient: Some(alternate.to_string()),
            },
        )
        .unwrap();
        let reply_id = res.messages[0].id;

        // Dispatched but not yet replied — FAILED_MINTS is empty.
        assert!(FAILED_MINTS
            .may_load(&deps.storage, &user)
            .unwrap()
            .is_none());

        // Simulate the alternate ALSO rejecting the mint.
        let r = synthetic_reply(reply_id, false, Some("alternate also blacklisted"));
        reply(deps.as_mut(), mock_env(), r).unwrap();

        // FAILED_MINTS re-credited under the ORIGINAL committer (`user`),
        // NOT under the alternate. The user can now try yet another
        // recipient on a fresh ClaimFailedDistribution call.
        assert_eq!(FAILED_MINTS.load(&deps.storage, &user).unwrap(), owed,);
        // Alternate has no FAILED_MINTS entry — they were a recipient
        // address only, never the canonical accounting key.
        assert!(FAILED_MINTS
            .may_load(&deps.storage, &alternate)
            .unwrap()
            .is_none());
    }

    /// Default recipient: when `recipient: None`, the mint is wired to
    /// the caller (committer) themselves. Useful for the "the recipient
    /// is fine again, just retry" case.
    #[test]
    fn claim_failed_distribution_defaults_recipient_to_caller() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);

        let user = Addr::unchecked("self_claim_committer");
        FAILED_MINTS
            .save(&mut deps.storage, &user, &Uint128::new(1))
            .unwrap();

        let info = message_info(&user, &[]);
        let res = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::ClaimFailedDistribution { recipient: None },
        )
        .unwrap();
        let sub = &res.messages[0];
        // Default recipient (recipient: None) mints to the caller. The mint
        // is a TokenFactory MsgMint now; compare against the exact builder
        // output for the caller as recipient.
        let expected = pool_core::osmosis_msgs::mint_msg(
            &Addr::unchecked("pool_contract"),
            CREATOR_DENOM,
            Uint128::new(1),
            &user,
        );
        assert_eq!(
            sub.msg, expected,
            "default recipient must be info.sender, got: {:?}",
            sub.msg
        );
    }

    /// Drained pool: every liveness primitive must reject so the
    /// post-drain invariant ("the pool no longer pays out from this
    /// contract") is uniform across all entry points.
    #[test]
    fn liveness_primitives_reject_on_drained_pool() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        install_factory(&mut deps);
        EMERGENCY_DRAINED.save(&mut deps.storage, &true).unwrap();

        // Self-recover
        let info = message_info(&Addr::unchecked("anyone"), &[]);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::SelfRecoverDistribution {},
        )
        .unwrap_err();
        assert!(
            format!("{:?}", err).contains("Drained") || format!("{:?}", err).contains("drained")
        );

        // Claim
        let info = message_info(&Addr::unchecked("anyone"), &[]);
        let err = execute(
            deps.as_mut(),
            mock_env(),
            info,
            ExecuteMsg::ClaimFailedDistribution { recipient: None },
        )
        .unwrap_err();
        assert!(
            format!("{:?}", err).contains("Drained") || format!("{:?}", err).contains("drained")
        );
    }

    /// Suppress unused-import lint in this test module — the timestamp
    /// import is referenced through `setup_pool_storage`'s internals.
    #[allow(dead_code)]
    fn _ts_marker(_t: Timestamp) {}
}
