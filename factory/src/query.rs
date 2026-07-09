use crate::asset::TokenType;
use crate::msg::FactoryInstantiateResponse;
use crate::state::{CreationStatus, FACTORYINSTANTIATEINFO, POOLS_BY_ID, POOL_CREATION_CONTEXT};
use cosmwasm_schema::{cw_serde, QueryResponses};
#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Addr, Binary, Deps, Env, Order, QueryRequest, StdResult, Timestamp, Uint128,
    WasmQuery,
};
use cw20::{Cw20QueryMsg, TokenInfoResponse};
use cw_storage_plus::Bound;
use pool_factory_interfaces::{FactoryQueryMsg, PoolKind};

#[cw_serde]
pub struct CreatorTokenInfoResponse {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: Uint128,
    pub token_address: Addr,
}

/// Per-pool creation diagnostics. Useful for off-chain tooling that watches
/// for stuck or repeatedly-failing pool creations and surfaces them to
/// operators. Returns `None` when the pool's creation state was already
/// cleaned up (i.e. creation succeeded end-to-end).
#[cw_serde]
pub struct PoolCreationStatusResponse {
    pub pool_id: u64,
    pub creator: Addr,
    pub creator_token_address: Option<Addr>,
    pub mint_new_position_nft_address: Option<Addr>,
    pub pool_address: Option<Addr>,
    pub creation_time: Timestamp,
    pub status: CreationStatus,
}

/// Default / maximum page sizes for `QueryMsg::Pools`. Mirrors the
/// bounds pattern used by the pool-side `PoolCommits` query so a single
/// call can't walk an unbounded range.
pub const POOLS_QUERY_DEFAULT_LIMIT: u32 = 30;
pub const POOLS_QUERY_MAX_LIMIT: u32 = 100;

/// One registry entry from `QueryMsg::Pools`.
#[cw_serde]
pub struct PoolListEntry {
    pub pool_id: u64,
    pub pool_addr: Addr,
    pub pool_token_info: [TokenType; 2],
    pub pool_kind: PoolKind,
}

#[cw_serde]
pub struct PoolsResponse {
    pub pools: Vec<PoolListEntry>,
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(FactoryInstantiateResponse)]
    Factory {},
    #[returns(CreatorTokenInfoResponse)]
    CreatorTokenInfo { pool_id: u64 },
    /// Cross-contract queries pools make against their factory
    /// (emergency-withdraw delay, live protocol wallet). Wraps the shared
    /// [`FactoryQueryMsg`] interface enum from `pool-factory-interfaces`.
    #[returns(cosmwasm_std::Binary)]
    PoolFactoryQuery(FactoryQueryMsg),
    /// Returns the in-flight creation status for a given pool_id, or None
    /// when creation completed cleanly and the entry was reaped.
    #[returns(Option<PoolCreationStatusResponse>)]
    PoolCreationStatus { pool_id: u64 },
    /// Registry lookup by pool *contract address*. Returns the pool's
    /// canonical pair + kind if `pool_addr` is a registered Bluechip pool,
    /// or `None` otherwise. Lets an integrator (notably the multi-hop
    /// router) validate an untrusted, caller-supplied pool address against
    /// the authoritative registry before sending funds to it.
    #[returns(Option<pool_factory_interfaces::RegisteredPoolResponse>)]
    PoolByAddress { pool_addr: String },
    /// Paginated registry enumeration, ordered by pool_id ascending.
    /// THE way for explorers and integrators to answer "what pools
    /// exist?" without an event indexer. Page with
    /// `start_after = last_entry.pool_id`; a page shorter than `limit`
    /// (default 30, max 100) signals end-of-data.
    #[returns(PoolsResponse)]
    Pools {
        start_after: Option<u64>,
        limit: Option<u32>,
    },
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Factory {} => to_json_binary(&query_active_factory(deps)?),
        QueryMsg::CreatorTokenInfo { pool_id } => {
            to_json_binary(&query_creator_token_info(deps, pool_id)?)
        }
        QueryMsg::PoolFactoryQuery(factory_msg) => {
            handle_pool_factory_query(deps, env, factory_msg)
        }
        QueryMsg::PoolCreationStatus { pool_id } => {
            to_json_binary(&query_pool_creation_status(deps, pool_id)?)
        }
        QueryMsg::PoolByAddress { pool_addr } => {
            to_json_binary(&query_pool_by_address(deps, pool_addr)?)
        }
        QueryMsg::Pools { start_after, limit } => {
            to_json_binary(&query_pools(deps, start_after, limit)?)
        }
    }
}

pub fn query_pools(
    deps: Deps,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<PoolsResponse> {
    let limit = limit
        .unwrap_or(POOLS_QUERY_DEFAULT_LIMIT)
        .min(POOLS_QUERY_MAX_LIMIT) as usize;
    let start = start_after.map(Bound::exclusive);
    let pools = POOLS_BY_ID
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            let (pool_id, details) = item?;
            Ok(PoolListEntry {
                pool_id,
                pool_addr: details.creator_pool_addr,
                pool_token_info: details.pool_token_info,
                pool_kind: details.pool_kind,
            })
        })
        .collect::<StdResult<Vec<_>>>()?;
    Ok(PoolsResponse { pools })
}

/// Resolve a pool *contract address* against the registry. Returns the
/// pool's canonical pair + kind, or `None` if the address is not a
/// registered Bluechip pool. Reuses the same `lookup_pool_by_addr` helper
/// the notify auth path uses, so a router validating a hop address
/// sees exactly the registry the factory itself trusts.
pub fn query_pool_by_address(
    deps: Deps,
    pool_addr: String,
) -> StdResult<Option<pool_factory_interfaces::RegisteredPoolResponse>> {
    let addr = deps.api.addr_validate(&pool_addr)?;
    let details = crate::state::lookup_pool_by_addr(deps, &addr)?;
    Ok(details.map(|d| pool_factory_interfaces::RegisteredPoolResponse {
        pool_id: d.pool_id,
        pool_token_info: d.pool_token_info,
        pool_kind: d.pool_kind,
    }))
}

pub fn query_pool_creation_status(
    deps: Deps,
    pool_id: u64,
) -> StdResult<Option<PoolCreationStatusResponse>> {
    let ctx = match POOL_CREATION_CONTEXT.may_load(deps.storage, pool_id)? {
        Some(c) => c,
        None => return Ok(None),
    };
    let crate::state::PoolCreationContext { temp, state } = ctx;
    Ok(Some(PoolCreationStatusResponse {
        pool_id: state.pool_id,
        creator: state.creator,
        // `ctx.temp` is now the single source of truth for these
        // addresses. The `state` mirrors that previously held them
        // were never written by current code paths and have been
        // removed; the wire-format response retains
        // `creator_token_address` / `mint_new_position_nft_address`
        // / `pool_address` slots so downstream consumers continue
        // to deserialize cleanly. `pool_address` is unset by the
        // current reply chain (no field on `temp` holds it yet);
        // populate when wiring becomes worthwhile.
        creator_token_address: temp.creator_token_addr,
        mint_new_position_nft_address: temp.nft_addr,
        pool_address: None,
        creation_time: state.creation_time,
        status: state.status,
    }))
}

pub fn query_creator_token_info(deps: Deps, pool_id: u64) -> StdResult<CreatorTokenInfoResponse> {
    let pool = POOLS_BY_ID.load(deps.storage, pool_id)?;

    let token_addr = pool
        .pool_token_info
        .iter()
        .find_map(|t| match t {
            TokenType::CreatorToken { contract_addr } => Some(contract_addr.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            cosmwasm_std::StdError::generic_err("No creator token found for this pool")
        })?;

    let token_info: TokenInfoResponse =
        deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
            contract_addr: token_addr.to_string(),
            msg: to_json_binary(&Cw20QueryMsg::TokenInfo {})?,
        }))?;

    Ok(CreatorTokenInfoResponse {
        name: token_info.name,
        symbol: token_info.symbol,
        decimals: token_info.decimals,
        total_supply: token_info.total_supply,
        token_address: token_addr,
    })
}

pub fn handle_pool_factory_query(
    deps: Deps,
    _env: Env,
    msg: FactoryQueryMsg,
) -> StdResult<Binary> {
    match msg {
        FactoryQueryMsg::EmergencyWithdrawDelaySeconds {} => {
            // Pools call this from `pool-core::execute_emergency_withdraw_initiate`
            // so the delay always tracks the live factory config rather
            // than a snapshot taken at pool instantiate.
            let cfg = FACTORYINSTANTIATEINFO.load(deps.storage)?;
            to_json_binary(&pool_factory_interfaces::EmergencyWithdrawDelayResponse {
                delay_seconds: cfg.emergency_withdraw_delay_seconds,
            })
        }
        FactoryQueryMsg::BluechipWalletAddress {} => {
            // Pools call this from `pool-core::execute_emergency_withdraw_core_drain`
            // to route Phase 2 sweep funds to the live wallet rather than
            // the snapshot taken in `COMMITFEEINFO.bluechip_wallet_address`
            // at pool instantiate. The factory's wallet is admin-tunable
            // through the standard 48h `ProposeConfigUpdate` flow.
            let cfg = FACTORYINSTANTIATEINFO.load(deps.storage)?;
            to_json_binary(&pool_factory_interfaces::BluechipWalletResponse {
                address: cfg.bluechip_wallet_address,
            })
        }
    }
}

pub fn query_active_factory(deps: Deps) -> StdResult<FactoryInstantiateResponse> {
    let factory = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    Ok(FactoryInstantiateResponse { factory })
}
