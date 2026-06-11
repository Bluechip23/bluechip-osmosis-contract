//! Multi-hop simulation logic.
//!
//! Read-only path that mirrors execution well enough for a frontend to
//! preview the final receive amount, every intermediate amount, and a
//! coarse price-impact estimate. The simulation walks the same route
//! the executor will and chains each pool's `Simulation` output into
//! the next hop's input.
//!
//! Cost: up to three queries per hop (factory registry lookup,
//! `IsFullyCommited` on commit pools only, `Simulation`), capped at
//! nine for a maximum-length route. One CONFIG storage read.

use cosmwasm_std::{to_json_binary, Decimal, Deps, QueryRequest, StdError, Uint128, WasmQuery};
use pool_factory_interfaces::asset::TokenInfo;
use pool_factory_interfaces::routing::{
    FactoryRouteQueryMsg, PoolSwapQueryMsg, RouterPoolCommitStatus,
    RouterSwapSimulationResponse, SwapOperation,
};
use pool_factory_interfaces::{PoolKind, RegisteredPoolResponse};

use crate::error::RouterError;
use crate::execution::validate_route;
use crate::msg::SimulateMultiHopResponse;
use crate::state::CONFIG;

/// Simulate a multi-hop route end to end.
///
/// For each hop the simulation
/// 1. resolves the pool against the factory registry — mirroring
/// execution's `validate_route_pools_registered`, and the only way to
/// learn the pool's kind without trusting the caller,
/// 2. for COMMIT pools only, queries `IsFullyCommited` and rejects if
/// the pool is still in its pre-threshold phase, so frontends never
/// render a silent zero result for a route that cannot yet execute.
/// (Standard pools do not implement this query — sending it
/// unconditionally made every standard-pool hop error out.)
/// 3. queries the pool's `Simulation` with the current chained input
/// and uses the returned amount as the next hop's input.
///
/// Price impact is reported as `1 - product(per_hop_survival)` where
/// `per_hop_survival = return_amount / (return_amount + spread_amount)`.
/// This captures cumulative pure slippage (ignoring LP fees) across
/// all hops; the multiplicative form gives correct compounding because
/// each hop's reduced output also reduces the next hop's input by the
/// same proportion. It is intentionally coarse and meant as a frontend
/// signal, not an exact mid-price comparison.
pub fn simulate_multi_hop(
    deps: Deps,
    operations: Vec<SwapOperation>,
    offer_amount: Uint128,
) -> Result<SimulateMultiHopResponse, RouterError> {
    if offer_amount.is_zero() {
        return Err(RouterError::ZeroAmount);
    }
    validate_route(&operations)?;

    let config = CONFIG.load(deps.storage)?;
    let mut current_input = offer_amount;
    let mut intermediate_amounts: Vec<Uint128> = Vec::with_capacity(operations.len());
    let mut survival = Decimal::one();

    for (idx, op) in operations.iter().enumerate() {
        let registered: Option<RegisteredPoolResponse> =
            deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
                contract_addr: config.factory_addr.to_string(),
                msg: to_json_binary(&FactoryRouteQueryMsg::PoolByAddress {
                    pool_addr: op.pool_addr.clone(),
                })?,
            }))?;
        let pool = registered.ok_or_else(|| RouterError::PoolNotRegistered {
            hop_index: idx,
            pool_addr: op.pool_addr.clone(),
        })?;

        if pool.pool_kind == PoolKind::Commit {
            let commit_status: RouterPoolCommitStatus =
                deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
                    contract_addr: op.pool_addr.clone(),
                    msg: to_json_binary(&PoolSwapQueryMsg::IsFullyCommited {})?,
                }))?;
            if let RouterPoolCommitStatus::InProgress { raised, target } = commit_status {
                return Err(RouterError::PoolInCommitPhase {
                    hop_index: idx,
                    pool_addr: op.pool_addr.clone(),
                    raised,
                    target,
                });
            }
        }

        let sim: RouterSwapSimulationResponse =
            deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
                contract_addr: op.pool_addr.clone(),
                msg: to_json_binary(&PoolSwapQueryMsg::Simulation {
                    offer_asset: TokenInfo {
                        info: op.offer_asset_info.clone(),
                        amount: current_input,
                    },
                })?,
            }))?;

        let ideal = sim
            .return_amount
            .checked_add(sim.spread_amount)
            .map_err(|_| RouterError::Std(StdError::generic_err("simulation amount overflow")))?;
        if !ideal.is_zero() {
            let factor = Decimal::from_ratio(sim.return_amount, ideal);
            survival = survival
                .checked_mul(factor)
                .map_err(|_| RouterError::Std(StdError::generic_err("price impact overflow")))?;
        }

        intermediate_amounts.push(sim.return_amount);
        current_input = sim.return_amount;
    }

    let final_amount = *intermediate_amounts.last().unwrap();
    let price_impact = Decimal::one()
        .checked_sub(survival)
        .unwrap_or(Decimal::zero());

    Ok(SimulateMultiHopResponse {
        final_amount,
        intermediate_amounts,
        price_impact,
    })
}
