//! Shared test fixtures for the creator-pool `#[cfg(test)]` suite.
//!
//! Phase-2: the internal AMM / LP-position system is gone. `PoolState`
//! shrank to just `pool_contract_address`; there is no `PoolFeeState`,
//! `Position`, or `NEXT_POSITION_ID`. These fixtures set up the surviving
//! commit-phase state and (post-threshold) a native GAMM `POOL_ID` so the
//! swap path has a pool to route through.

use cosmwasm_std::testing::{
    MockApi, MockQuerier, MockStorage,
};
use cosmwasm_std::{
    to_json_binary, Addr, Binary, ContractResult, Decimal, OwnedDeps, SystemError, SystemResult,
    Uint128, WasmQuery,
};

use crate::asset::{PoolPairType, TokenType};
use crate::msg::CommitFeeInfo;
use crate::state::{
    CommitLimitInfo, PoolDetails, PoolInfo, PoolSpecs, PoolState, ThresholdPayoutAmounts,
    COMMITFEEINFO, COMMIT_LIMIT_INFO, IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_ID,
    POOL_INFO, POOL_SPECS, POOL_STATE, THRESHOLD_PAYOUT_AMOUNTS, THRESHOLD_PROCESSING,
    USD_RAISED_FROM_COMMIT,
};

/// The pool's native creator TokenFactory denom used across the shared
/// test fixtures. Post-migration the creator token is a bank denom
/// (`factory/{pool_addr}/{subdenom}`) rather than a CW20 contract; the
/// setup helpers pin the pool address as "pool_contract", so its creator
/// denom is deterministic.
pub const CREATOR_DENOM: &str = "factory/pool_contract/ucreator";

/// A `mock_dependencies` whose contract bank balance is seeded with
/// `balances`.
pub fn mock_dependencies_with_balance(
    balances: &[cosmwasm_std::Coin],
) -> OwnedDeps<MockStorage, MockApi, MockQuerier> {
    let mut deps = cosmwasm_std::testing::mock_dependencies();
    deps.querier
        .bank
        .update_balance(cosmwasm_std::testing::MOCK_CONTRACT_ADDR, balances.to_vec());
    deps
}

/// Sets up a pool in pre-threshold state with all surviving configuration.
pub fn setup_pool_storage(deps: &mut OwnedDeps<MockStorage, MockApi, MockQuerier>) {
    let pool_info = PoolInfo {
        pool_id: 1u64,
        pool_info: PoolDetails {
            asset_infos: [
                TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                TokenType::CreatorToken {
                    denom: CREATOR_DENOM.to_string(),
                },
            ],
            contract_addr: Addr::unchecked("pool_contract"),
            pool_type: PoolPairType::Xyk {},
        },
        factory_addr: Addr::unchecked("factory_contract"),
        token_denom: CREATOR_DENOM.to_string(),
    };
    POOL_INFO.save(&mut deps.storage, &pool_info).unwrap();

    let pool_state = PoolState {
        pool_contract_address: Addr::unchecked("pool_contract"),
    };
    POOL_STATE.save(&mut deps.storage, &pool_state).unwrap();

    let pool_specs = PoolSpecs {
        lp_fee: Decimal::percent(3) / Uint128::new(10), // 0.3% fee (3/1000)
        min_commit_interval: 60,                        // 1 minute minimum between commits
    };
    POOL_SPECS.save(&mut deps.storage, &pool_specs).unwrap();

    let commit_config = CommitLimitInfo {
        commit_amount_for_threshold_usd: Uint128::new(25_000_000_000), // 25k native with 6 decimals
        max_bluechip_lock_per_pool: Uint128::new(10_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        min_commit_usd_pre_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_PRE_THRESHOLD,
        min_commit_usd_post_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_POST_THRESHOLD,
    };
    COMMIT_LIMIT_INFO
        .save(&mut deps.storage, &commit_config)
        .unwrap();

    let threshold_payout = ThresholdPayoutAmounts {
        creator_reward_amount: Uint128::new(325_000_000_000),
        bluechip_reward_amount: Uint128::new(25_000_000_000),
        pool_seed_amount: Uint128::new(350_000_000_000),
        commit_return_amount: Uint128::new(500_000_000_000),
    };
    THRESHOLD_PAYOUT_AMOUNTS
        .save(&mut deps.storage, &threshold_payout)
        .unwrap();

    let commit_fee_info = CommitFeeInfo {
        bluechip_wallet_address: Addr::unchecked("bluechip_treasury"),
        creator_wallet_address: Addr::unchecked("creator_wallet"),
        commit_fee_bluechip: Decimal::percent(1), // 1%
        commit_fee_creator: Decimal::percent(5),  // 5%
    };
    COMMITFEEINFO
        .save(&mut deps.storage, &commit_fee_info)
        .unwrap();

    THRESHOLD_PROCESSING
        .save(&mut deps.storage, &false)
        .unwrap();
    IS_THRESHOLD_HIT.save(&mut deps.storage, &false).unwrap();
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::zero())
        .unwrap();
    NATIVE_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::zero())
        .unwrap();
}

/// Post-threshold pool: threshold hit, USD raised at target, and a native
/// GAMM `POOL_ID` set so `SimpleSwap` / post-threshold commits can route
/// their `MsgSwapExactAmountIn` through it.
pub fn setup_pool_post_threshold(deps: &mut OwnedDeps<MockStorage, MockApi, MockQuerier>) {
    setup_pool_storage(deps);
    IS_THRESHOLD_HIT.save(&mut deps.storage, &true).unwrap();
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(25_000_000_000))
        .unwrap();
    // The native pool id learned from the MsgCreateBalancerPool reply at
    // threshold-crossing. Seed a stable id so swaps route.
    POOL_ID.save(&mut deps.storage, &1u64).unwrap();
}

/// Installs a mock factory USD-valuation responder at the given rate
/// (micro-USD per micro-native; 1_000_000 = $1 per token). Answers both
/// `ConvertNativeToUsd` and the commit path's `CommitContext` (whose
/// `bluechip_wallet` matches the `bluechip_treasury` snapshot pinned by
/// `setup_pool_storage`). All other cross-contract queries error.
pub fn with_factory_oracle(
    deps: &mut OwnedDeps<MockStorage, MockApi, MockQuerier>,
    native_to_usd_rate: Uint128,
) {
    deps.querier.update_wasm(move |query| match query {
        WasmQuery::Smart { msg, .. } => {
            #[cosmwasm_schema::cw_serde]
            enum WrapperProbe {
                PoolFactoryQuery(pool_factory_interfaces::FactoryQueryMsg),
            }
            let usd_at_rate = |amount: Uint128| {
                amount
                    .checked_mul(native_to_usd_rate)
                    .unwrap()
                    .checked_div(Uint128::new(1_000_000))
                    .unwrap()
            };
            match cosmwasm_std::from_json(msg) {
                Ok(WrapperProbe::PoolFactoryQuery(
                    pool_factory_interfaces::FactoryQueryMsg::ConvertNativeToUsd { amount },
                )) => {
                    let resp = pool_factory_interfaces::ConversionResponse {
                        amount: usd_at_rate(amount),
                        rate_used: native_to_usd_rate,
                        timestamp: 0,
                    };
                    return SystemResult::Ok(ContractResult::Ok(to_json_binary(&resp).unwrap()));
                }
                Ok(WrapperProbe::PoolFactoryQuery(
                    pool_factory_interfaces::FactoryQueryMsg::CommitContext { amount },
                )) => {
                    let resp = pool_factory_interfaces::CommitContextResponse {
                        amount: usd_at_rate(amount),
                        rate_used: native_to_usd_rate,
                        timestamp: 0,
                        bluechip_wallet: Addr::unchecked("bluechip_treasury"),
                    };
                    return SystemResult::Ok(ContractResult::Ok(to_json_binary(&resp).unwrap()));
                }
                _ => {}
            }
            SystemResult::Err(SystemError::InvalidRequest {
                error: "no other cross-contract queries expected".to_string(),
                request: msg.clone(),
            })
        }
        _ => SystemResult::Err(SystemError::InvalidRequest {
            error: "unsupported wasm query".to_string(),
            request: Binary::default(),
        }),
    });
}
