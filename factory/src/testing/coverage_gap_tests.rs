//! Coverage-gap tests for factory paths that had no Rust regression cover
//! prior to this file:
//!
//! - `must_pay` surplus refund on commit-pool `Create`.

use cosmwasm_std::testing::{message_info, mock_env, MockApi, MockStorage};
use cosmwasm_std::{Addr, BankMsg, Coin, CosmosMsg, Decimal, OwnedDeps, Uint128};

use crate::asset::TokenType;
use crate::execute::{execute, instantiate};
use crate::mock_querier::{mock_dependencies, WasmMockQuerier};
use crate::msg::{CreatorTokenInfo, ExecuteMsg};
use crate::pool_struct::CreatePool;
use crate::state::FactoryInstantiate;

// --- shared helpers --------------------------------------------------------

fn make_addr(label: &str) -> Addr {
    MockApi::default().addr_make(label)
}

fn admin() -> Addr {
    make_addr("admin")
}

/// Flat creation fee (native base units) configured in
/// `default_factory_config`. There is no oracle conversion or fallback
/// anymore — this exact amount is what `Create` requires.
const CREATION_FEE: u128 = 1_000_000;

fn default_factory_config() -> FactoryInstantiate {
    FactoryInstantiate {
        cw721_nft_contract_id: 58,
        factory_admin_address: admin(),
        commit_threshold_limit_usd: Uint128::new(25_000_000_000),
        cw20_token_contract_id: 10,
        create_pool_wasm_contract_id: 11,
        standard_pool_wasm_contract_id: 0,
        bluechip_wallet_address: make_addr("ubluechip"),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 14,
        bluechip_denom: "ubluechip".to_string(),
        pricing_pool_id: 1,
        usd_quote_denom: "uusdc".to_string(),
        twap_window_seconds: 600,
        standard_pool_creation_fee: Uint128::new(CREATION_FEE),
        threshold_payout_amounts: Default::default(),
        emergency_withdraw_delay_seconds: 86_400,
    }
}

fn fresh_factory() -> OwnedDeps<MockStorage, MockApi, WasmMockQuerier> {
    let mut deps = mock_dependencies(&[]);
    instantiate(
        deps.as_mut(),
        mock_env(),
        message_info(&admin(), &[]),
        default_factory_config(),
    )
    .unwrap();
    deps
}

// ---------------------------------------------------------------------------
// must_pay surplus refund
// ---------------------------------------------------------------------------

/// Commit-pool `Create` enforces `must_pay` on the bluechip denom and the
/// flat configured fee (`standard_pool_creation_fee`, native base units).
/// Overpaying that amount must produce a Bank `Send` refunding the surplus
/// to `info.sender` inside the same response.
#[test]
fn create_pool_refunds_surplus_to_sender() {
    let mut deps = fresh_factory();

    let required: u128 = CREATION_FEE;
    let surplus: u128 = 50_000_000;
    let paid = Uint128::new(required + surplus);

    let funds = vec![Coin {
        denom: "ubluechip".to_string(),
        amount: paid,
    }];
    let info = message_info(&admin(), &funds);

    let create_msg = ExecuteMsg::Create {
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
            name: "RefundToken".to_string(),
            symbol: "REFUND".to_string(),
            decimal: 6,
        },
    };

    let res = execute(deps.as_mut(), mock_env(), info, create_msg).unwrap();

    // Exactly one BankMsg::Send must address the sender with the surplus
    // amount of ubluechip. (The other potential BankMsg from this
    // response — the fee transfer — addresses the bluechip wallet, not
    // the sender.)
    let admin_addr_str = admin().to_string();
    let refund_match = res.messages.iter().find_map(|sub| match &sub.msg {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount }) if to_address == &admin_addr_str => {
            amount
                .iter()
                .find(|c| c.denom == "ubluechip" && c.amount == Uint128::new(surplus))
                .map(|_| ())
        }
        _ => None,
    });
    assert!(
        refund_match.is_some(),
        "expected BankMsg::Send refunding {} ubluechip to {}, got {:?}",
        surplus,
        admin_addr_str,
        res.messages
    );
}

/// Negative complement: paying *exactly* the required fee must NOT emit
/// any BankMsg targeting `info.sender` — the surplus branch is guarded on
/// `!surplus.is_zero()`.
#[test]
fn create_pool_exact_pay_emits_no_refund() {
    let mut deps = fresh_factory();

    let funds = vec![Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(CREATION_FEE),
    }];
    let info = message_info(&admin(), &funds);

    let create_msg = ExecuteMsg::Create {
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
            name: "ExactToken".to_string(),
            symbol: "EXACT".to_string(),
            decimal: 6,
        },
    };

    let res = execute(deps.as_mut(), mock_env(), info, create_msg).unwrap();

    let admin_addr_str = admin().to_string();
    let any_refund = res.messages.iter().any(|sub| {
        matches!(&sub.msg, CosmosMsg::Bank(BankMsg::Send { to_address, .. }) if to_address == &admin_addr_str)
    });
    assert!(
        !any_refund,
        "exact-pay create must not emit a refund BankMsg to sender; got {:?}",
        res.messages
    );
}
