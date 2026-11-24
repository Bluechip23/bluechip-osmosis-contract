//! `WithdrawCommit` against the real chain: a pool that has not crossed
//! its threshold after 18 months lets each committer take their net
//! contribution back, with a real bank transfer and real block time.

mod common;

use common::*;
use cosmwasm_std::{Coin, Uint128};
use creator_pool::msg::{
    CommitStatus, CommitWithdrawInfoResponse, ExecuteMsg as PoolExecuteMsg,
    QueryMsg as PoolQueryMsg,
};
use osmosis_test_tube::{Account, Bank, Gamm, Module, OsmosisTestApp, Wasm};

const LOCK_SECONDS: u64 = 548 * 86_400;

#[test]
fn committer_withdraws_from_an_uncrossed_pool_after_18_months() {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&coins_sorted(vec![
            Coin::new(1_000_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ]))
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let alice = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let bob = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();

    let gamm = Gamm::new(&app);
    let pricing_pool_id = gamm
        .create_basic_pool(
            &coins_sorted(vec![
                Coin::new(1_000_000_000u128, UOSMO),
                Coin::new(1_000_000_000u128, UUSDC),
            ]),
            &admin,
        )
        .unwrap()
        .data
        .pool_id;
    app.increase_time(700);

    let wasm = Wasm::new(&app);
    let (factory_code_id, pool_code_id) = store_factory_and_pool(&wasm, &admin);
    let factory_addr = instantiate_factory(
        &wasm,
        factory_code_id,
        &factory_init(
            &admin.address(),
            pricing_pool_id,
            pool_code_id,
            Coin::new(GAMM_CREATE_FEE, UOSMO),
        ),
        &admin,
    );
    let (pool_addr, _denom, _id) = create_creator_pool(&wasm, &factory_addr, &creator, "REFUND");
    let bank = Bank::new(&app);

    // Two committers, well short of the 25,000 OSMO threshold.
    wasm.execute(
        &pool_addr,
        &commit_msg(1_000_000_000, None),
        &[Coin::new(1_000_000_000u128, UOSMO)],
        &alice,
    )
    .unwrap();
    wasm.execute(
        &pool_addr,
        &commit_msg(2_000_000_000, None),
        &[Coin::new(2_000_000_000u128, UOSMO)],
        &bob,
    )
    .unwrap();

    let info: CommitWithdrawInfoResponse = wasm
        .query(
            &pool_addr,
            &PoolQueryMsg::CommitWithdrawInfo {
                wallet: Some(alice.address()),
            },
        )
        .unwrap();
    assert!(!info.withdrawals_open);
    assert_eq!(info.committed, Uint128::new(1_000_000_000));
    assert_eq!(info.refundable, Uint128::new(940_000_000));

    // Too early: refused.
    let err = wasm
        .execute(&pool_addr, &PoolExecuteMsg::WithdrawCommit {}, &[], &alice)
        .unwrap_err();
    assert!(
        err.to_string().contains("18 months"),
        "early withdrawal must be refused, got: {err}"
    );

    // 18 months later the pool still has not crossed.
    app.increase_time(LOCK_SECONDS);
    let pool_before = balance(&bank, &pool_addr, UOSMO);

    let res = wasm
        .execute(&pool_addr, &PoolExecuteMsg::WithdrawCommit {}, &[], &alice)
        .expect("withdrawal after 18 months");

    // The pool paid out exactly Alice's net contribution (94% of 1,000)...
    assert_eq!(
        pool_before - balance(&bank, &pool_addr, UOSMO),
        Uint128::new(940_000_000)
    );
    // ...to Alice (read from the bank transfer event; her balance delta
    // also includes the test chain's tx fee, so it is not asserted).
    let paid_to_alice = res.events.iter().any(|e| {
        e.ty == "transfer"
            && e.attributes
                .iter()
                .any(|a| a.key == "recipient" && a.value == alice.address())
            && e.attributes
                .iter()
                .any(|a| a.key == "sender" && a.value == pool_addr)
            && e.attributes
                .iter()
                .any(|a| a.key == "amount" && a.value == "940000000uosmo")
    });
    assert!(paid_to_alice, "refund transfer to alice not found: {:?}", res.events);

    // Her gross came off the amount committed toward the threshold.
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::InProgress { raised, .. } if raised == Uint128::new(2_000_000_000)),
        "committed total must drop to Bob's 2,000 OSMO, got {status:?}"
    );

    // One-shot; Bob's own withdrawal still works afterwards.
    wasm.execute(&pool_addr, &PoolExecuteMsg::WithdrawCommit {}, &[], &alice)
        .unwrap_err();
    wasm.execute(&pool_addr, &PoolExecuteMsg::WithdrawCommit {}, &[], &bob)
        .expect("second committer withdraws");
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(matches!(status, CommitStatus::InProgress { raised, .. } if raised.is_zero()));
}
