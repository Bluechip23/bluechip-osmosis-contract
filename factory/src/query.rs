use crate::asset::TokenType;
use crate::msg::FactoryInstantiateResponse;
use crate::state::{CreationStatus, FACTORYINSTANTIATEINFO, POOLS_BY_ID};
use cosmwasm_schema::{cw_serde, QueryResponses};
#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Addr, Binary, Deps, Env, Order, StdResult, Timestamp, Uint128,
};
use cw_storage_plus::Bound;
use pool_factory_interfaces::FactoryQueryMsg;

#[cw_serde]
pub struct CreatorTokenInfoResponse {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: Uint128,
    /// The creator token's native TokenFactory bank denom. (Pre-migration
    /// this was `token_address: Addr`, the CW20 contract address.)
    pub token_denom: String,
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
    /// Every in-flight 48h-timelocked change, in one read. The timelock's
    /// security value is community observability — dashboards and watchers
    /// need a first-class way to ask "what is pending?" without raw-KV
    /// reads or an event indexer. Empty/None everywhere ⇒ nothing pending.
    #[returns(PendingChangesResponse)]
    PendingChanges {},
}

#[cosmwasm_schema::cw_serde]
pub struct PendingChangesResponse {
    /// Pending factory-config replacement (`ProposeConfigUpdate`).
    pub config: Option<crate::state::PendingConfig>,
    /// Pending router registration/rotation (`ProposeRouter`).
    pub router: Option<crate::state::PendingRouter>,
    /// Pending batched pool wasm upgrade (`ProposePoolUpgrade`).
    pub pool_upgrade: Option<crate::state::PoolUpgrade>,
    /// Pending per-pool config updates (`ProposePoolConfigUpdate`),
    /// as (pool_id, pending) pairs. Capped at 100 entries per read.
    pub pool_configs: Vec<(u64, crate::state::PendingPoolConfig)>,
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
        QueryMsg::PendingChanges {} => {
            let pool_configs = crate::state::PENDING_POOL_CONFIG
                .range(
                    deps.storage,
                    None,
                    None,
                    cosmwasm_std::Order::Ascending,
                )
                .take(100)
                .collect::<StdResult<Vec<_>>>()?;
            to_json_binary(&PendingChangesResponse {
                config: crate::state::PENDING_CONFIG.may_load(deps.storage)?,
                router: crate::state::PENDING_ROUTER.may_load(deps.storage)?,
                pool_upgrade: crate::state::PENDING_POOL_UPGRADE.may_load(deps.storage)?,
                pool_configs,
            })
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
            })
        })
        .collect::<StdResult<Vec<_>>>()?;
    Ok(PoolsResponse { pools })
}

/// Resolve a pool *contract address* against the registry. Returns the
/// pool's canonical pair, or `None` if the address is not a
/// registered Bluechip pool. Reuses the same `lookup_pool_by_addr` helper
/// the notify auth path uses, so a router validating a hop address
/// sees exactly the registry the factory itself trusts.
pub fn query_pool_by_address(
    deps: Deps,
    pool_addr: String,
) -> StdResult<Option<pool_factory_interfaces::RegisteredPoolResponse>> {
    let addr = deps.api.addr_validate(&pool_addr)?;
    let details = crate::state::lookup_pool_by_addr(deps, &addr)?;
    Ok(
        details.map(|d| pool_factory_interfaces::RegisteredPoolResponse {
            pool_id: d.pool_id,
            pool_token_info: d.pool_token_info,
        }),
    )
}

pub fn query_pool_creation_status(
    _deps: Deps,
    _pool_id: u64,
) -> StdResult<Option<PoolCreationStatusResponse>> {
    // Pool creation is atomic within a single tx: the creation context
    // rides the SubMsg payloads through the reply chain, every step is
    // `reply_on_success`, and a failure anywhere reverts the whole tx.
    // There is therefore never an externally observable "in-flight"
    // creation — a pool either exists in the registry (query
    // `PoolDetails` / `PoolByAddress`) or was never created. The query
    // and its response shape are retained for wire compatibility and
    // always report no in-flight creation.
    Ok(None)
}

pub fn query_creator_token_info(deps: Deps, pool_id: u64) -> StdResult<CreatorTokenInfoResponse> {
    let pool = POOLS_BY_ID.load(deps.storage, pool_id)?;

    let token_denom = pool
        .pool_token_info
        .iter()
        .find_map(|t| match t {
            TokenType::CreatorToken { denom } => Some(denom.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            cosmwasm_std::StdError::generic_err("No creator token found for this pool")
        })?;

    // The creator token is a native TokenFactory denom now — there is no
    // CW20 contract to smart-query for name/symbol. Total supply comes
    // from the bank module; name/symbol are not carried on-chain for a
    // bare TokenFactory denom (they'd require a separate x/bank metadata
    // record), so they are reported empty here. Decimals are fixed at 6
    // (the protocol calibrates the threshold payout / mint caps for
    // 6-decimal creator tokens; see `validate_creator_token_info`).
    let total_supply = deps
        .querier
        .query_supply(token_denom.clone())
        .map(|c| c.amount)
        .unwrap_or_else(|_| Uint128::zero());

    Ok(CreatorTokenInfoResponse {
        name: String::new(),
        symbol: String::new(),
        decimals: 6,
        total_supply,
        token_denom,
    })
}

pub fn handle_pool_factory_query(deps: Deps, _env: Env, msg: FactoryQueryMsg) -> StdResult<Binary> {
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
        FactoryQueryMsg::CommitContext { include_fee_budget } => {
            // Single round-trip for the pool commit path. The threshold is
            // native-denominated so there is no valuation here — just the
            // live factory context a commit needs: wallet, gamm fee coin,
            // and the TWAP-valued native budget for acquiring a non-native
            // fee at crossing. Fail-closed: a TWAP error propagates and
            // the commit reverts.
            //
            // The crossing charges the chain's LIVE `x/poolmanager` fee
            // (the pool resolves live params first, this config second),
            // so BOTH the returned fee coin and the budget sizing track
            // the live value where available: config drift (admin set a
            // zero/native fee while the chain charges USDC) or a chain-
            // governance fee change can then never strand a crossing
            // without a budget or leave the reserve sized to the wrong
            // coin. Budget is valued at max(configured, live) of any
            // quote-denominated fee — over-reserving is safe (surplus is
            // remitted at crossing), under-reserving bricks retryably.
            //
            // `include_fee_budget: Some(false)` (sent on post-threshold
            // commits, which never fund a fee swap) skips the TWAP read so
            // a pricing-pool outage cannot block trading-phase commits.
            let cfg = FACTORYINSTANTIATEINFO.load(deps.storage)?;
            let cfg_fee = Some(cfg.gamm_pool_creation_fee.clone())
                .filter(|c| !c.amount.is_zero());
            let live_fee = crate::fee_twap::query_live_pool_creation_fee(&deps.querier)
                .filter(|c| !c.amount.is_zero());
            let quote_fee_amount = [cfg_fee.as_ref(), live_fee.as_ref()]
                .into_iter()
                .flatten()
                .filter(|c| c.denom == cfg.fee_quote_denom)
                .map(|c| c.amount)
                .max()
                .unwrap_or_default();
            let fee_swap_budget_native = if include_fee_budget.unwrap_or(true)
                && !quote_fee_amount.is_zero()
            {
                Some(crate::fee_twap::fee_swap_budget_native(
                    deps,
                    &_env,
                    &cfg,
                    quote_fee_amount,
                )?)
            } else {
                None
            };
            to_json_binary(&pool_factory_interfaces::CommitContextResponse {
                timestamp: _env.block.time.seconds(),
                bluechip_wallet: cfg.bluechip_wallet_address,
                gamm_pool_creation_fee: live_fee.or(cfg_fee),
                fee_swap_budget_native,
                pricing_pool_id: cfg.pricing_pool_id,
                fee_quote_denom: cfg.fee_quote_denom,
            })
        }
        FactoryQueryMsg::RegisteredRouter {} => {
            // Pools call this on a null-belief SimpleSwap to check
            // whether the caller is the exempt router. `None` when unset.
            to_json_binary(&pool_factory_interfaces::RegisteredRouterResponse {
                router: crate::state::ROUTER_ADDRESS.may_load(deps.storage)?,
            })
        }
    }
}

pub fn query_active_factory(deps: Deps) -> StdResult<FactoryInstantiateResponse> {
    let factory = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    Ok(FactoryInstantiateResponse { factory })
}
