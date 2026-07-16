//! Commit-only creator claim / factory-notify handlers.
//!
//! Phase-2: the internal LP system and the creator fee-pot are gone, so
//! `ClaimCreatorFees` / `CREATOR_FEE_POT` no longer exist and their tests
//! were removed. What survives here:
//! - `execute_retry_factory_notify` — re-sends NotifyThresholdCrossed
//!   to the factory when the initial submsg's reply_on_error handler
//!   set PENDING_FACTORY_NOTIFY=true.
//! - the `reply` dispatch for the factory-notify reply ids.

use crate::state::PENDING_FACTORY_NOTIFY;
use cosmwasm_std::testing::{message_info, mock_dependencies, mock_env};
use cosmwasm_std::{Addr, CosmosMsg, SubMsg, WasmMsg};

use crate::contract::{execute_retry_factory_notify};
use crate::error::ContractError;
use crate::testing::fixtures::setup_pool_storage;

// -- execute_retry_factory_notify ---------------------------------------

#[test]
fn retry_factory_notify_dispatches_submsg_when_pending() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    // Arm the pending flag (production flow sets this from the
    // reply_on_error handler when the initial factory notify fails).
    PENDING_FACTORY_NOTIFY
        .save(&mut deps.storage, &true)
        .unwrap();
    // Production flow sets THRESHOLD_CROSSED_AT inside
    // `trigger_threshold_payout` alongside IS_THRESHOLD_HIT. The retry
    // handler `load`s it (not `may_load`) so the snapshot must be
    // present whenever PENDING_FACTORY_NOTIFY is true.
    crate::state::THRESHOLD_CROSSED_AT
        .save(&mut deps.storage, &mock_env().block.time)
        .unwrap();

    // Anyone can call RetryFactoryNotify — factory's idempotency gates
    // double-processing.
    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let res = execute_retry_factory_notify(deps.as_mut(), mock_env(), info).unwrap();

    // Response carries one submessage targeting the factory contract.
    assert_eq!(res.messages.len(), 1);
    let sub: &SubMsg = &res.messages[0];
    match &sub.msg {
        CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr, msg, ..
        }) => {
            assert_eq!(contract_addr, "factory_contract");
            let body = String::from_utf8_lossy(msg.as_slice());
            assert!(body.contains("notify_threshold_crossed"));
            assert!(body.contains("\"pool_id\":1"));
        }
        other => panic!("expected WasmMsg::Execute, got {:?}", other),
    }

    assert!(res
        .attributes
        .iter()
        .any(|a| a.key == "action" && a.value == "retry_factory_notify"));
}

#[test]
fn retry_factory_notify_rejects_when_no_pending() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    // PENDING_FACTORY_NOTIFY unset — default reads as `false`.

    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let err = execute_retry_factory_notify(deps.as_mut(), mock_env(), info).unwrap_err();
    assert!(matches!(err, ContractError::NoPendingFactoryNotify));
}

#[test]
fn retry_factory_notify_rejects_when_flag_false() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    PENDING_FACTORY_NOTIFY
        .save(&mut deps.storage, &false)
        .unwrap();

    let info = message_info(&Addr::unchecked("anyone"), &[]);
    let err = execute_retry_factory_notify(deps.as_mut(), mock_env(), info).unwrap_err();
    assert!(matches!(err, ContractError::NoPendingFactoryNotify));
}

// -- reply handler -------------------------------------------------------
//
// The pool's `reply` entry point handles the factory-notify reply IDs:
// - REPLY_ID_FACTORY_NOTIFY_INITIAL (reply_on_error): on Err, sets
//   PENDING_FACTORY_NOTIFY so RetryFactoryNotify can be invoked later.
// - REPLY_ID_FACTORY_NOTIFY_RETRY (reply_always): on Ok, clears
//   PENDING_FACTORY_NOTIFY; on Err keeps it set.

mod reply_handler_tests {
    use super::*;
    use crate::contract::reply;
    use crate::state::{REPLY_ID_FACTORY_NOTIFY_INITIAL, REPLY_ID_FACTORY_NOTIFY_RETRY};
    use cosmwasm_std::{Binary, Reply, SubMsgResponse, SubMsgResult};

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
                SubMsgResult::Err(err_msg.unwrap_or("synthetic failure").to_string())
            },
        }
    }

    #[test]
    fn reply_initial_notify_on_error_sets_pending_flag() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        assert!(!PENDING_FACTORY_NOTIFY
            .may_load(&deps.storage)
            .unwrap()
            .unwrap_or(false));

        let r = synthetic_reply(
            REPLY_ID_FACTORY_NOTIFY_INITIAL,
            false,
            Some("factory rejected: pool not registered"),
        );
        let res = reply(deps.as_mut(), mock_env(), r).expect("reply must Ok on Err result");

        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "factory_notify_deferred"));
        assert!(res.attributes.iter().any(
            |a| a.key == "reason" && a.value.contains("factory rejected: pool not registered")
        ));
        assert!(PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
    }

    #[test]
    fn reply_initial_notify_on_ok_is_noop() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        PENDING_FACTORY_NOTIFY
            .save(&mut deps.storage, &true)
            .unwrap();

        let r = synthetic_reply(REPLY_ID_FACTORY_NOTIFY_INITIAL, true, None);
        let res = reply(deps.as_mut(), mock_env(), r).expect("Ok branch must return Ok response");

        assert!(res.attributes.is_empty());
        assert!(PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
    }

    #[test]
    fn reply_retry_on_ok_clears_pending_flag() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        PENDING_FACTORY_NOTIFY
            .save(&mut deps.storage, &true)
            .unwrap();

        let r = synthetic_reply(REPLY_ID_FACTORY_NOTIFY_RETRY, true, None);
        let res = reply(deps.as_mut(), mock_env(), r).expect("retry success path must Ok");

        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "factory_notify_retry_succeeded"));
        assert!(!PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
    }

    #[test]
    fn reply_retry_on_error_keeps_pending_flag() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);
        PENDING_FACTORY_NOTIFY
            .save(&mut deps.storage, &true)
            .unwrap();

        let r = synthetic_reply(REPLY_ID_FACTORY_NOTIFY_RETRY, false, Some("factory paused"));
        let res = reply(deps.as_mut(), mock_env(), r)
            .expect("retry failure must NOT propagate as Err — gas-trap risk");

        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "action" && a.value == "factory_notify_retry_failed"));
        assert!(res
            .attributes
            .iter()
            .any(|a| a.key == "reason" && a.value.contains("factory paused")));
        assert!(PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
    }

    #[test]
    fn reply_unknown_id_returns_error() {
        let mut deps = mock_dependencies();
        setup_pool_storage(&mut deps);

        let r = synthetic_reply(0xDEADBEEF, true, None);
        let err = reply(deps.as_mut(), mock_env(), r).unwrap_err();
        assert!(err.to_string().contains("unknown reply id"));
    }

    /// State-snapshot atomicity: after `REPLY_ID_FACTORY_NOTIFY_INITIAL`
    /// fires with Err, only PENDING_FACTORY_NOTIFY may differ. Every
    /// other crossing-mutated storage must be byte-identical.
    #[test]
    fn reply_initial_notify_err_does_not_touch_crossing_state() {
        use crate::state::{
            IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_STATE, USD_RAISED_FROM_COMMIT,
        };
        use crate::testing::fixtures::setup_pool_post_threshold;
        use cosmwasm_std::Uint128;

        let mut deps = mock_dependencies();
        setup_pool_post_threshold(&mut deps);

        NATIVE_RAISED_FROM_COMMIT
            .save(&mut deps.storage, &Uint128::new(123_456_789))
            .unwrap();

        let snap_pool_state = POOL_STATE.load(&deps.storage).unwrap();
        let snap_is_threshold_hit = IS_THRESHOLD_HIT.load(&deps.storage).unwrap();
        let snap_usd_raised = USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
        let snap_native_raised = NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
        assert!(!PENDING_FACTORY_NOTIFY
            .may_load(&deps.storage)
            .unwrap()
            .unwrap_or(false));

        let r = synthetic_reply(
            REPLY_ID_FACTORY_NOTIFY_INITIAL,
            false,
            Some("simulated factory rejection"),
        );
        reply(deps.as_mut(), mock_env(), r).expect("reply must Ok on Err result");

        assert!(
            PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap(),
            "PENDING_FACTORY_NOTIFY must be armed after notify failure"
        );
        assert_eq!(POOL_STATE.load(&deps.storage).unwrap(), snap_pool_state);
        assert_eq!(
            IS_THRESHOLD_HIT.load(&deps.storage).unwrap(),
            snap_is_threshold_hit
        );
        assert_eq!(
            USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_usd_raised
        );
        assert_eq!(
            NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_native_raised
        );
    }

    #[test]
    fn reply_retry_err_does_not_touch_crossing_state() {
        use crate::state::{
            IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_STATE, USD_RAISED_FROM_COMMIT,
        };
        use crate::testing::fixtures::setup_pool_post_threshold;
        use cosmwasm_std::Uint128;

        let mut deps = mock_dependencies();
        setup_pool_post_threshold(&mut deps);
        NATIVE_RAISED_FROM_COMMIT
            .save(&mut deps.storage, &Uint128::new(123_456_789))
            .unwrap();
        PENDING_FACTORY_NOTIFY
            .save(&mut deps.storage, &true)
            .unwrap();

        let snap_pool_state = POOL_STATE.load(&deps.storage).unwrap();
        let snap_is_threshold_hit = IS_THRESHOLD_HIT.load(&deps.storage).unwrap();
        let snap_usd_raised = USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
        let snap_native_raised = NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();

        let r = synthetic_reply(REPLY_ID_FACTORY_NOTIFY_RETRY, false, Some("still failing"));
        reply(deps.as_mut(), mock_env(), r)
            .expect("retry failure must NOT propagate — gas-trap risk");

        assert!(PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
        assert_eq!(POOL_STATE.load(&deps.storage).unwrap(), snap_pool_state);
        assert_eq!(
            IS_THRESHOLD_HIT.load(&deps.storage).unwrap(),
            snap_is_threshold_hit
        );
        assert_eq!(
            USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_usd_raised
        );
        assert_eq!(
            NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_native_raised
        );
    }

    #[test]
    fn reply_retry_ok_does_not_touch_crossing_state() {
        use crate::state::{
            IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_STATE, USD_RAISED_FROM_COMMIT,
        };
        use crate::testing::fixtures::setup_pool_post_threshold;
        use cosmwasm_std::Uint128;

        let mut deps = mock_dependencies();
        setup_pool_post_threshold(&mut deps);
        NATIVE_RAISED_FROM_COMMIT
            .save(&mut deps.storage, &Uint128::new(123_456_789))
            .unwrap();
        PENDING_FACTORY_NOTIFY
            .save(&mut deps.storage, &true)
            .unwrap();

        let snap_pool_state = POOL_STATE.load(&deps.storage).unwrap();
        let snap_is_threshold_hit = IS_THRESHOLD_HIT.load(&deps.storage).unwrap();
        let snap_usd_raised = USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
        let snap_native_raised = NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();

        let r = synthetic_reply(REPLY_ID_FACTORY_NOTIFY_RETRY, true, None);
        reply(deps.as_mut(), mock_env(), r).expect("retry success must Ok");

        assert!(!PENDING_FACTORY_NOTIFY.load(&deps.storage).unwrap());
        assert_eq!(POOL_STATE.load(&deps.storage).unwrap(), snap_pool_state);
        assert_eq!(
            IS_THRESHOLD_HIT.load(&deps.storage).unwrap(),
            snap_is_threshold_hit
        );
        assert_eq!(
            USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_usd_raised
        );
        assert_eq!(
            NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
            snap_native_raised
        );
    }
}
