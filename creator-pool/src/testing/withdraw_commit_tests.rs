//! `WithdrawCommit`: the committer exit from a pool that has not crossed
//! its threshold after 18 months.

use cosmwasm_std::testing::{message_info, mock_env};
use cosmwasm_std::{from_json, Addr, BankMsg, Coin, CosmosMsg, Env, OwnedDeps, Uint128};

use crate::asset::{TokenInfo, TokenType};
use crate::contract::execute;
use crate::error::ContractError;
use crate::msg::{CommitWithdrawInfoResponse, ExecuteMsg, QueryMsg};
use crate::state::{
    COMMITTER_COUNT, COMMIT_INFO, COMMIT_LEDGER, COMMIT_NET_LEDGER, COMMIT_WITHDRAW_LOCK_SECONDS,
    GROSS_NATIVE_COMMITTED, IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_PAUSED,
};
use crate::testing::fixtures::{
    mock_dependencies_with_balance, setup_pool_storage, with_factory_context,
};

const DENOM: &str = "ubluechip";

type Deps = OwnedDeps<
    cosmwasm_std::testing::MockStorage,
    cosmwasm_std::testing::MockApi,
    cosmwasm_std::testing::MockQuerier,
>;

fn setup() -> Deps {
    let mut deps = mock_dependencies_with_balance(&[Coin::new(100_000_000_000u128, DENOM)]);
    setup_pool_storage(&mut deps);
    with_factory_context(&mut deps);
    deps
}

fn commit(deps: &mut Deps, env: &Env, who: &str, amount: u128) {
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&Addr::unchecked(who), &[Coin::new(amount, DENOM)]),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: DENOM.to_string(),
                },
                amount: Uint128::new(amount),
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap();
}

fn withdraw(
    deps: &mut Deps,
    env: &Env,
    who: &str,
) -> Result<cosmwasm_std::Response, ContractError> {
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(&Addr::unchecked(who), &[]),
        ExecuteMsg::WithdrawCommit {},
    )
}

fn after_lock() -> Env {
    let mut env = mock_env();
    env.block.time = env.block.time.plus_seconds(COMMIT_WITHDRAW_LOCK_SECONDS);
    env
}

#[test]
fn withdrawal_is_locked_until_the_pool_is_18_months_old() {
    let mut deps = setup();
    let env = mock_env();
    commit(&mut deps, &env, "alice", 1_000_000_000);

    // One second short of 18 months: refused, nothing changes.
    let mut early = mock_env();
    early.block.time = early
        .block
        .time
        .plus_seconds(COMMIT_WITHDRAW_LOCK_SECONDS - 1);
    let err = withdraw(&mut deps, &early, "alice").unwrap_err();
    assert!(matches!(err, ContractError::CommitWithdrawLocked { .. }));
    assert_eq!(
        GROSS_NATIVE_COMMITTED.load(&deps.storage).unwrap(),
        Uint128::new(1_000_000_000)
    );

    // Exactly at 18 months: accepted.
    withdraw(&mut deps, &after_lock(), "alice").unwrap();
}

#[test]
fn withdrawal_refunds_net_and_subtracts_from_the_committed_total() {
    let mut deps = setup();
    let env = mock_env();
    commit(&mut deps, &env, "alice", 1_000_000_000);
    let mut later = mock_env();
    later.block.time = later.block.time.plus_seconds(60);
    commit(&mut deps, &later, "alice", 500_000_000); // second commit, same wallet
    commit(&mut deps, &env, "bob", 2_000_000_000);

    assert_eq!(
        GROSS_NATIVE_COMMITTED.load(&deps.storage).unwrap(),
        Uint128::new(3_500_000_000)
    );
    let net_before = NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap();
    assert_eq!(net_before, Uint128::new(3_290_000_000)); // 94% of 3.5k
    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 2);

    let alice = Addr::unchecked("alice");
    let res = withdraw(&mut deps, &after_lock(), "alice").unwrap();

    // Alice gets her NET back: 94% of 1.5k (the 6% fees were paid out at
    // commit time and are not held by the pool).
    assert_eq!(res.messages.len(), 1);
    match &res.messages[0].msg {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount }) => {
            assert_eq!(to_address, alice.as_str());
            assert_eq!(amount, &vec![Coin::new(1_410_000_000u128, DENOM)]);
        }
        other => panic!("expected a bank refund, got {other:?}"),
    }

    // Her GROSS comes off the committed total; her net off the raised total.
    assert_eq!(
        GROSS_NATIVE_COMMITTED.load(&deps.storage).unwrap(),
        Uint128::new(2_000_000_000)
    );
    assert_eq!(
        NATIVE_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(),
        Uint128::new(1_880_000_000)
    );
    // She is gone from every ledger; Bob is untouched.
    assert!(!COMMIT_LEDGER.has(&deps.storage, &alice));
    assert!(!COMMIT_NET_LEDGER.has(&deps.storage, &alice));
    assert!(!COMMIT_INFO.has(&deps.storage, &alice));
    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 1);
    let bob = Addr::unchecked("bob");
    assert_eq!(
        COMMIT_LEDGER.load(&deps.storage, &bob).unwrap(),
        Uint128::new(2_000_000_000)
    );
    assert_eq!(
        COMMIT_NET_LEDGER.load(&deps.storage, &bob).unwrap(),
        Uint128::new(1_880_000_000)
    );

    // One-shot.
    let err = withdraw(&mut deps, &after_lock(), "alice").unwrap_err();
    assert!(matches!(err, ContractError::NoCommitToWithdraw { .. }));
    // Someone who never committed has nothing to withdraw.
    let err = withdraw(&mut deps, &after_lock(), "carol").unwrap_err();
    assert!(matches!(err, ContractError::NoCommitToWithdraw { .. }));

    // The pool stays open: Alice can commit again and is counted afresh.
    let mut again = after_lock();
    again.block.time = again.block.time.plus_seconds(60);
    commit(&mut deps, &again, "alice", 1_000_000_000);
    assert_eq!(
        GROSS_NATIVE_COMMITTED.load(&deps.storage).unwrap(),
        Uint128::new(3_000_000_000)
    );
    assert_eq!(COMMITTER_COUNT.load(&deps.storage).unwrap(), 2);
}

#[test]
fn withdrawal_is_closed_once_the_pool_has_crossed() {
    let mut deps = setup();
    let env = mock_env();
    commit(&mut deps, &env, "alice", 1_000_000_000);
    commit(&mut deps, &env, "whale", 30_000_000_000); // crosses the 25k threshold
    assert!(IS_THRESHOLD_HIT.load(&deps.storage).unwrap());

    let err = withdraw(&mut deps, &after_lock(), "alice").unwrap_err();
    assert!(matches!(
        err,
        ContractError::CommitWithdrawAfterThreshold {}
    ));
    assert!(COMMIT_LEDGER.has(&deps.storage, &Addr::unchecked("alice")));
}

#[test]
fn withdrawal_works_while_paused_and_rejects_attached_funds() {
    let mut deps = setup();
    let env = mock_env();
    commit(&mut deps, &env, "alice", 1_000_000_000);

    // Attached funds are refused.
    let err = execute(
        deps.as_mut(),
        after_lock(),
        message_info(&Addr::unchecked("alice"), &[Coin::new(1u128, DENOM)]),
        ExecuteMsg::WithdrawCommit {},
    )
    .unwrap_err();
    assert!(matches!(err, ContractError::InvalidCommitFunds { .. }));

    // An admin pause cannot block the committer's exit.
    POOL_PAUSED.save(&mut deps.storage, &true).unwrap();
    withdraw(&mut deps, &after_lock(), "alice").unwrap();
    assert_eq!(
        GROSS_NATIVE_COMMITTED.load(&deps.storage).unwrap(),
        Uint128::zero()
    );
}

#[test]
fn commit_withdraw_info_query_reports_unlock_and_refund() {
    let mut deps = setup();
    let env = mock_env();
    // The query validates the wallet, so use a well-formed mock address.
    let alice_addr = deps.api.addr_make("alice");
    let alice = alice_addr.as_str();
    commit(&mut deps, &env, alice, 1_000_000_000);

    let q = |deps: &Deps, env: Env, wallet: Option<&str>| -> CommitWithdrawInfoResponse {
        from_json(
            crate::query::query(
                deps.as_ref(),
                env,
                QueryMsg::CommitWithdrawInfo {
                    wallet: wallet.map(|w| w.to_string()),
                },
            )
            .unwrap(),
        )
        .unwrap()
    };

    let info = q(&deps, mock_env(), Some(alice));
    assert_eq!(
        info.unlock_time,
        mock_env()
            .block
            .time
            .plus_seconds(COMMIT_WITHDRAW_LOCK_SECONDS)
    );
    assert!(!info.withdrawals_open);
    assert_eq!(info.committed, Uint128::new(1_000_000_000));
    assert_eq!(info.refundable, Uint128::new(940_000_000));

    let info = q(&deps, after_lock(), Some(alice));
    assert!(info.withdrawals_open);
    let info = q(&deps, after_lock(), None);
    assert_eq!(info.refundable, Uint128::zero());
}
