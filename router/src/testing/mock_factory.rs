//! Test-only mock factory contract for router integration tests.
//!
//! The router validates every hop's `pool_addr` against the factory's
//! pool registry before moving funds (see
//! `router::execution::validate_route_pools_registered`). Production uses
//! the real factory's `PoolByAddress` query; the router tests stand up
//! this minimal stand-in that answers the same query from an explicit
//! allowlist supplied at instantiate. This keeps the router tests focused
//! on routing without dragging the full factory + reply chain into the
//! harness.
//!
//! Not part of the production build -- lives under `#[cfg(test)]`.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    entry_point, to_json_binary, Binary, Deps, DepsMut, Empty, Env, MessageInfo, Response,
    StdResult,
};
use cw_storage_plus::Map;
use pool_factory_interfaces::asset::TokenType;
use pool_factory_interfaces::{PoolKind, RegisteredPoolResponse};

/// One registered pool: its contract address and canonical pair. The
/// harness builds one of these per mock pool it stands up.
#[cw_serde]
pub struct RegistryEntry {
    pub pool_addr: String,
    pub pool_token_info: [TokenType; 2],
    /// Defaults to Commit (the historical assumption); tests standing up
    /// standard-pool hops set `Standard` so the router's registry-driven
    /// commit-status gating can be exercised.
    #[serde(default)]
    pub pool_kind: PoolKind,
}

#[cw_serde]
pub struct InstantiateMsg {
    pub pools: Vec<RegistryEntry>,
}

/// Mirrors the variant the router sends
/// (`pool_factory_interfaces::routing::FactoryRouteQueryMsg::PoolByAddress`),
/// which is itself byte-identical to `factory::query::QueryMsg::PoolByAddress`.
#[cw_serde]
pub enum QueryMsg {
    PoolByAddress { pool_addr: String },
}

/// addr (bech32 string the router passes through) -> (canonical pair, kind).
const POOLS: Map<&str, ([TokenType; 2], PoolKind)> = Map::new("pools");

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> StdResult<Response> {
    for entry in msg.pools {
        POOLS.save(
            deps.storage,
            &entry.pool_addr,
            &(entry.pool_token_info, entry.pool_kind),
        )?;
    }
    Ok(Response::new())
}

/// No-op: the router only ever *queries* the factory. Present because
/// `ContractWrapper` requires an execute entry point.
#[entry_point]
pub fn execute(_deps: DepsMut, _env: Env, _info: MessageInfo, _msg: Empty) -> StdResult<Response> {
    Ok(Response::new())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        // Returns `Some(..)` for a registered pool, `None` otherwise —
        // exactly the shape the real factory returns, so the router's
        // `Option<RegisteredPoolResponse>` decode is identical here.
        QueryMsg::PoolByAddress { pool_addr } => {
            let resp =
                POOLS
                    .may_load(deps.storage, &pool_addr)?
                    .map(|(pool_token_info, pool_kind)| RegisteredPoolResponse {
                        pool_id: 0,
                        pool_token_info,
                        pool_kind,
                    });
            to_json_binary(&resp)
        }
    }
}
