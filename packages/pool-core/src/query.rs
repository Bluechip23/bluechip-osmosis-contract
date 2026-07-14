//! Shared query handlers.
//!
//! Phase-2: the internal AMM is gone. Reserves, positions, price
//! accumulators and internal fee accounting no longer exist locally.
//! Queries that used to read that state now either:
//!  - route to the native Osmosis pool (e.g. `Simulation` via the
//!    poolmanager `estimate_swap_exact_amount_in` query), or
//!  - return zero/default for a retained wire field, documented inline.
//!
//! `query_analytics` is factored: `query_analytics_core` assembles the
//! shared-state portion of `PoolAnalyticsResponse`; the consuming
//! contract's wrapper supplies the commit-adjacent fields.

use crate::asset::TokenInfo;
use crate::msg::{
    CommitStatus, ConfigResponse, FeeInfoResponse, PoolAnalyticsResponse, PoolFeeStateResponse,
    PoolInfoResponse, PoolStateResponse, SimulationResponse,
};
use crate::state::{
    PoolDetails, COMMITFEEINFO, IS_THRESHOLD_HIT, POOL_ANALYTICS, POOL_ID, POOL_INFO, POOL_PAUSED,
    POOL_STATE,
};
use cosmwasm_std::{to_json_binary, Binary, Deps, Env, StdError, StdResult, Uint128};
use std::str::FromStr;
use osmosis_std::types::osmosis::poolmanager::v1beta1::{PoolmanagerQuerier, SwapAmountInRoute};
use pool_factory_interfaces::{
    AllPoolsResponse, IsPausedResponse, PoolQueryMsg, PoolStateResponseForFactory,
};

pub fn query_is_paused(deps: Deps) -> StdResult<IsPausedResponse> {
    let paused = POOL_PAUSED.may_load(deps.storage)?.unwrap_or(false);
    Ok(IsPausedResponse { paused })
}

pub fn query_pair_info(deps: Deps) -> StdResult<PoolDetails> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    Ok(pool_info.pool_info)
}

/// Extract the denom string of a `TokenType`.
fn denom_of(t: &crate::asset::TokenType) -> String {
    use crate::asset::TokenType;
    match t {
        TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom.clone(),
    }
}

/// Simulate a swap against the NATIVE Osmosis pool via the poolmanager
/// `estimate_swap_exact_amount_in` query. `spread_amount` /
/// `commission_amount` are not returned by that estimate, so they are
/// reported as zero — callers wanting the full breakdown must inspect the
/// native pool directly. Errors (pre-threshold pool with no `POOL_ID`, or
/// an estimate query failure) propagate.
pub fn query_simulation(deps: Deps, offer_asset: TokenInfo) -> StdResult<SimulationResponse> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    let infos = &pool_info.pool_info.asset_infos;

    let (offer_denom, ask_denom) = if offer_asset.info.equal(&infos[0]) {
        (denom_of(&infos[0]), denom_of(&infos[1]))
    } else if offer_asset.info.equal(&infos[1]) {
        (denom_of(&infos[1]), denom_of(&infos[0]))
    } else {
        return Err(StdError::generic_err(
            "Given offer asset does not belong in the pair",
        ));
    };

    let pool_id = POOL_ID.may_load(deps.storage)?.ok_or_else(|| {
        StdError::generic_err("Pool has not been seeded yet (pre-threshold); nothing to quote")
    })?;

    let querier = PoolmanagerQuerier::new(&deps.querier);
    let token_in = format!("{}{}", offer_asset.amount, offer_denom);
    let resp = querier.estimate_swap_exact_amount_in(
        String::new(),
        pool_id,
        token_in,
        vec![SwapAmountInRoute {
            pool_id,
            token_out_denom: ask_denom,
        }],
    )?;
    let return_amount = Uint128::from_str(&resp.token_out_amount)
        .map_err(|e| StdError::generic_err(format!("invalid estimate token_out_amount: {}", e)))?;

    Ok(SimulationResponse {
        return_amount,
        spread_amount: Uint128::zero(),
        commission_amount: Uint128::zero(),
    })
}

pub fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    // `block_time_last` was part of the retired internal price accumulator;
    // reported as zero now.
    let _ = deps;
    Ok(ConfigResponse {
        block_time_last: 0,
        params: None,
    })
}

pub fn query_fee_info(deps: Deps) -> StdResult<FeeInfoResponse> {
    let fee_info = COMMITFEEINFO.load(deps.storage)?;
    Ok(FeeInfoResponse { fee_info })
}

/// Returns true only after the threshold crossing has fully completed
/// (IS_THRESHOLD_HIT == true). Gates all post-threshold operations.
pub fn query_check_commit(deps: Deps) -> StdResult<bool> {
    IS_THRESHOLD_HIT.load(deps.storage)
}

/// Best-effort read of the pool's held liquidity on the native Osmosis
/// pool: `(reserve0, reserve1)` matching `asset_infos` order. Returns
/// `(0, 0)` when the pool has not been seeded or the native query fails
/// (keeps queries robust in unit-test environments with no gamm module).
fn native_reserves(deps: Deps) -> (Uint128, Uint128) {
    let Ok(pool_info) = POOL_INFO.load(deps.storage) else {
        return (Uint128::zero(), Uint128::zero());
    };
    let Ok(Some(pool_id)) = POOL_ID.may_load(deps.storage) else {
        return (Uint128::zero(), Uint128::zero());
    };
    let querier = PoolmanagerQuerier::new(&deps.querier);
    let Ok(resp) = querier.total_pool_liquidity(pool_id) else {
        return (Uint128::zero(), Uint128::zero());
    };
    let denom0 = denom_of(&pool_info.pool_info.asset_infos[0]);
    let denom1 = denom_of(&pool_info.pool_info.asset_infos[1]);
    let find = |d: &str| -> Uint128 {
        resp.liquidity
            .iter()
            .find(|c| c.denom == d)
            .and_then(|c| Uint128::from_str(&c.amount).ok())
            .unwrap_or_default()
    };
    (find(&denom0), find(&denom1))
}

pub fn query_pool_state(deps: Deps) -> StdResult<PoolStateResponse> {
    let (reserve0, reserve1) = native_reserves(deps);
    Ok(PoolStateResponse {
        // `nft_ownership_accepted` is retained for wire compatibility; the
        // position-NFT integration was removed in Phase-2.
        nft_ownership_accepted: false,
        reserve0,
        reserve1,
        // total_liquidity is the pool-held gamm LP-share amount; not
        // surfaced here (query the native pool by `gamm/pool/{id}` denom).
        total_liquidity: Uint128::zero(),
        block_time_last: 0,
    })
}

pub fn query_fee_state(_deps: Deps) -> StdResult<PoolFeeStateResponse> {
    // Internal fee-growth accounting was removed; swap fees accrue
    // natively to the pool-held seed LP. Reported as zero.
    Ok(PoolFeeStateResponse {
        fee_growth_global_0: cosmwasm_std::Decimal::zero(),
        fee_growth_global_1: cosmwasm_std::Decimal::zero(),
        total_fees_collected_0: Uint128::zero(),
        total_fees_collected_1: Uint128::zero(),
    })
}

pub fn query_pool_info(deps: Deps) -> StdResult<PoolInfoResponse> {
    let (reserve0, reserve1) = native_reserves(deps);
    Ok(PoolInfoResponse {
        pool_state: PoolStateResponse {
            nft_ownership_accepted: false,
            reserve0,
            reserve1,
            total_liquidity: Uint128::zero(),
            block_time_last: 0,
        },
        fee_state: PoolFeeStateResponse {
            fee_growth_global_0: cosmwasm_std::Decimal::zero(),
            fee_growth_global_1: cosmwasm_std::Decimal::zero(),
            total_fees_collected_0: Uint128::zero(),
            total_fees_collected_1: Uint128::zero(),
        },
        total_positions: 0,
    })
}

/// Assembles the parts of `PoolAnalyticsResponse` that don't depend on
/// commit-phase state.
pub fn query_analytics_core(
    deps: Deps,
    threshold_status: CommitStatus,
    total_usd_raised: Uint128,
    total_bluechip_raised: Uint128,
) -> StdResult<PoolAnalyticsResponse> {
    let analytics = POOL_ANALYTICS.may_load(deps.storage)?.unwrap_or_default();
    let (reserve0, reserve1) = native_reserves(deps);

    let current_price_0_to_1 = if !reserve0.is_zero() {
        cosmwasm_std::Decimal::from_ratio(reserve1, reserve0).to_string()
    } else {
        "0".to_string()
    };
    let current_price_1_to_0 = if !reserve1.is_zero() {
        cosmwasm_std::Decimal::from_ratio(reserve0, reserve1).to_string()
    } else {
        "0".to_string()
    };

    Ok(PoolAnalyticsResponse {
        analytics,
        current_price_0_to_1,
        current_price_1_to_0,
        total_value_locked_0: reserve0,
        total_value_locked_1: reserve1,
        // Internal fee-reserve accounting removed; fees accrue natively.
        fee_reserve_0: Uint128::zero(),
        fee_reserve_1: Uint128::zero(),
        threshold_status,
        total_usd_raised,
        total_bluechip_raised,
        total_positions: 0,
    })
}

/// Build the factory response struct from current pool state.
fn build_factory_response(deps: Deps) -> StdResult<PoolStateResponseForFactory> {
    let pool_state = POOL_STATE.load(deps.storage)?;
    let pool_info = POOL_INFO.load(deps.storage)?;
    let (reserve0, reserve1) = native_reserves(deps);
    let assets: Vec<String> = pool_info
        .pool_info
        .asset_infos
        .iter()
        .map(|a| a.to_string())
        .collect();

    Ok(PoolStateResponseForFactory {
        pool_contract_address: pool_state.pool_contract_address,
        nft_ownership_accepted: false,
        reserve0,
        reserve1,
        total_liquidity: Uint128::zero(),
        block_time_last: 0,
        price0_cumulative_last: Uint128::zero(),
        price1_cumulative_last: Uint128::zero(),
        assets,
    })
}

pub fn query_for_factory(deps: Deps, _env: Env, msg: PoolQueryMsg) -> StdResult<Binary> {
    match msg {
        PoolQueryMsg::GetPoolState {} => to_json_binary(&build_factory_response(deps)?),
        PoolQueryMsg::GetAllPools {} => {
            let response = build_factory_response(deps)?;
            to_json_binary(&AllPoolsResponse {
                pools: vec![(response.pool_contract_address.to_string(), response)],
            })
        }
        PoolQueryMsg::IsPaused {} => to_json_binary(&query_is_paused(deps)?),
    }
}
