//! Factory contract entry points + shared reply-ID machinery.
//!
//! The bulk of the handler logic has been split into submodules
//! by message family:
//!
//! - [`config`]         — propose / apply / cancel for both factory
//! config and per-pool config (48h timelock on
//! every propose/apply pair).
//! - [`pool_lifecycle`] — create, pause, unpause,
//! emergency withdraw (+ cancel), stuck-state
//! recovery, and the threshold-crossed
//! callback from pools.
//! - [`upgrades`]       — pool wasm upgrade proposal + batched migrate
//! apply.
//!
//! This file keeps the `#[entry_point]` exports (`instantiate`,
//! `execute`, `reply`), the cross-module helpers (`ensure_admin`,
//! `encode_reply_id`, `decode_reply_id`), and the reply-step
//! constants. Every other public item in `crate::execute` is
//! re-exported from a submodule via `pub use`.

pub mod config;
pub mod pool_lifecycle;
pub mod upgrades;

// Explicit re-exports keep the public surface of `crate::execute::*`
// traceable from this file rather than implicitly extending whenever a
// submodule adds a new `pub fn`. Adding a handler requires touching
// the dispatcher in this file, which keeps the two in step.
pub use config::{
    execute_apply_pool_config_update, execute_cancel_factory_config_update,
    execute_cancel_pool_config_update, execute_propose_factory_config_update,
    execute_propose_pool_config_update, execute_update_factory_config,
};
// `validate_factory_config` is intentionally NOT re-exported — it's
// reached via the `config::validate_factory_config(...)` path in
// `instantiate` so the gate is visible at the call site.
pub use pool_lifecycle::admin::{
    execute_cancel_emergency_withdraw_pool, execute_emergency_withdraw_pool,
    execute_notify_threshold_crossed, execute_pause_pool, execute_recover_pool_stuck_states,
    execute_unpause_pool,
};
pub use upgrades::{
    execute_apply_pool_upgrade, execute_cancel_pool_upgrade, execute_continue_pool_upgrade,
    execute_propose_pool_upgrade,
};

use crate::error::ContractError;
use crate::msg::ExecuteMsg;
use crate::pool_creation_reply::finalize_pool;
use crate::state::FACTORYINSTANTIATEINFO;
#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{Deps, DepsMut, Env, MessageInfo, Reply, Response};

use crate::{CONTRACT_NAME, CONTRACT_VERSION};

// Reply step constants (stored in low 8 bits of reply ID).
//
// Phase-2: the CW20-instantiate step AND the position-NFT-instantiate step
// are both gone. The creator token is a pool-owned native denom and the
// internal LP system was removed, so the reply chain is a single step:
// pool-instantiate -> finalize. `FINALIZE_POOL` handles the pool-created
// reply. (`MINT_CREATE_POOL` = 2 is retired; the constant is left out so a
// stale reply id routes to `UnknownReplyId`.)
pub const FINALIZE_POOL: u64 = 3;

/// Encodes a `pool_id` and a reply-chain step into a single SubMsg reply ID.
///
/// Layout: low 8 bits = step, high 56 bits = pool_id.
/// Step IDs MUST fit in 8 bits (0..=0xFF). Pool IDs are bumped by a single
/// counter per pool create and so cannot reach 2^56 in any realistic
/// deployment, but the asserts keep these invariants explicit so a future
/// step-constant change above 0xFF or a malformed pool_id is caught in
/// debug builds before it silently truncates and routes to UnknownReplyId.
pub fn encode_reply_id(pool_id: u64, step: u64) -> u64 {
    debug_assert!(step <= 0xFF, "reply step {} does not fit in 8 bits", step);
    debug_assert!(
        pool_id < (1u64 << 56),
        "pool_id {} risks truncation in reply id",
        pool_id
    );
    (pool_id << 8) | (step & 0xFF)
}

/// Decodes a reply ID back into `(pool_id, step)`.
pub fn decode_reply_id(reply_id: u64) -> (u64, u64) {
    (reply_id >> 8, reply_id & 0xFF)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    msg: crate::state::FactoryInstantiate,
) -> Result<Response, ContractError> {
    cw2::set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    config::validate_factory_config(deps.as_ref(), &env, &msg)?;

    FACTORYINSTANTIATEINFO.save(deps.storage, &msg)?;
    // M-05 — a fresh deployment maintains PAIRS / POOL_ID_BY_ADDRESS through
    // `register_pool` from the first pool onward, so the legacy registry
    // back-fill in `migrate` is never needed. Mark it done up front so
    // `migrate` skips the O(N) walk entirely for this deployment.
    crate::state::REGISTRY_BACKFILL_DONE.save(deps.storage, &true)?;
    Ok(Response::new().add_attribute("action", "init_contract"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::ProposeConfigUpdate { config } => {
            execute_propose_factory_config_update(deps, env, info, config)
        }
        ExecuteMsg::UpdateConfig {} => execute_update_factory_config(deps, env, info),
        ExecuteMsg::SetRouter { router } => execute_set_router(deps, info, router),
        ExecuteMsg::CancelConfigUpdate {} => execute_cancel_factory_config_update(deps, info),
        ExecuteMsg::Create {
            pool_msg,
            token_info,
        } => pool_lifecycle::create::execute_create_creator_pool(
            deps, env, info, pool_msg, token_info,
        ),
        ExecuteMsg::UpgradePools {
            new_code_id,
            pool_ids,
            migrate_msg,
        } => execute_propose_pool_upgrade(deps, env, info, new_code_id, pool_ids, migrate_msg),
        ExecuteMsg::ExecutePoolUpgrade {} => execute_apply_pool_upgrade(deps, env, info),
        ExecuteMsg::CancelPoolUpgrade {} => execute_cancel_pool_upgrade(deps, info),
        ExecuteMsg::ContinuePoolUpgrade {} => execute_continue_pool_upgrade(deps, env, info),
        ExecuteMsg::ProposePoolConfigUpdate {
            pool_id,
            pool_config,
        } => execute_propose_pool_config_update(deps, env, info, pool_id, pool_config),
        ExecuteMsg::ExecutePoolConfigUpdate { pool_id } => {
            execute_apply_pool_config_update(deps, env, info, pool_id)
        }
        ExecuteMsg::CancelPoolConfigUpdate { pool_id } => {
            execute_cancel_pool_config_update(deps, info, pool_id)
        }
        ExecuteMsg::NotifyThresholdCrossed {
            pool_id,
            crossed_at,
        } => execute_notify_threshold_crossed(deps, env, info, pool_id, crossed_at),
        ExecuteMsg::PausePool { pool_id } => execute_pause_pool(deps, info, pool_id),
        ExecuteMsg::UnpausePool { pool_id } => execute_unpause_pool(deps, info, pool_id),
        ExecuteMsg::EmergencyWithdrawPool { pool_id } => {
            execute_emergency_withdraw_pool(deps, info, pool_id)
        }
        ExecuteMsg::CancelEmergencyWithdrawPool { pool_id } => {
            execute_cancel_emergency_withdraw_pool(deps, info, pool_id)
        }
        ExecuteMsg::RecoverPoolStuckStates {
            pool_id,
            recovery_type,
        } => execute_recover_pool_stuck_states(deps, info, pool_id, recovery_type),
        ExecuteMsg::PruneRateLimits { batch_size } => {
            execute_prune_rate_limits(deps, env, batch_size)
        }
    }
}

/// Permissionless storage hygiene. Iterates the
/// per-address rate-limit maps and removes entries older than 10× the
/// per-map cooldown window.
///
/// `batch_size` caps the number of entries REMOVED per call (default
/// 100, hard cap 500). It does NOT cap the number iterated — each
/// phase walks its map until either `batch_size` stale entries have
/// been collected or the map ends. For realistic deployment scales
/// this is fine: per-address 1h cooldowns mean the map can't grow
/// faster than ~24 entries/day/active-address, and the keeper runs
/// prune ~daily, so the map stays small enough that full iteration
/// is well within block gas. If the map ever does balloon (extended
/// prune outage, deliberate storage-bloat attack), operators tune
/// the keeper to run more frequently AND can manually invoke this
/// handler with larger `batch_size` to drain the backlog.
///
/// Without this handler, the rate-limit maps grow monotonically as
/// new addresses interact and never shrink. Pruning is anybody's
/// job: ops, keepers, or any community member can run it.
fn execute_prune_rate_limits(
    deps: DepsMut,
    env: Env,
    batch_size: Option<u32>,
) -> Result<Response, ContractError> {
    let batch = batch_size.unwrap_or(100).min(500) as usize;
    let now_secs = env.block.time.seconds();

    // 10× the cooldown (1h today → 10h), well beyond any legitimate
    // user's natural retry cadence.
    let stale_after_secs = crate::state::COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS.saturating_mul(10);

    let commit_pruned = prune_rate_limit_map(
        deps.storage,
        crate::state::LAST_COMMIT_POOL_CREATE_AT,
        crate::state::COMMIT_POOL_CREATE_TS_INDEX,
        now_secs,
        stale_after_secs,
        batch,
    )?;
    Ok(Response::new()
        .add_attribute("action", "prune_rate_limits")
        .add_attribute("commit_pruned", commit_pruned.to_string())
        .add_attribute("stale_after_secs", stale_after_secs.to_string())
        .add_attribute("batch_size", batch.to_string()))
}

/// Prune up to `batch` entries from a per-address `Addr -> Timestamp`
/// rate-limit map whose timestamp is older than
/// `now_secs - stale_after_secs`. Returns the number of entries actually
/// removed (`<= batch`). Centralized so adding a third such map is a
/// one-line addition rather than a
/// copy-pasted loop with risk of attribute-key drift.
///
/// The walk is timestamp-ordered via the secondary `(ts, addr)` index,
/// NOT alphabetic over `map`. Because the index is sorted ascending
/// by timestamp, the first entry whose timestamp is younger than the
/// stale threshold guarantees every later entry is also fresh — we
/// break out immediately. Worst-case work is therefore O(stale_count)
/// regardless of overall map size; an O(N) walk over the alphabetic
/// primary could visit every entry before finding the first stale
/// one.
fn prune_rate_limit_map(
    storage: &mut dyn cosmwasm_std::Storage,
    primary: cw_storage_plus::Map<cosmwasm_std::Addr, cosmwasm_std::Timestamp>,
    ts_index: cw_storage_plus::Map<(u64, cosmwasm_std::Addr), ()>,
    now_secs: u64,
    stale_after_secs: u64,
    batch: usize,
) -> cosmwasm_std::StdResult<u32> {
    use cosmwasm_std::Order;

    let mut to_remove: Vec<(u64, cosmwasm_std::Addr)> = Vec::new();
    for entry in ts_index.range(storage, None, None, Order::Ascending) {
        if to_remove.len() >= batch {
            break;
        }
        let ((ts, addr), _) = entry?;
        if now_secs.saturating_sub(ts) >= stale_after_secs {
            to_remove.push((ts, addr));
        } else {
            // Ascending-timestamp iteration: the first fresh entry
            // guarantees no later entry is stale. Break early instead
            // of paying for the rest of the walk.
            break;
        }
    }
    let mut pruned: u32 = 0;
    for (ts, addr) in to_remove.into_iter() {
        primary.remove(storage, addr.clone());
        ts_index.remove(storage, (ts, addr));
        pruned = pruned.saturating_add(1);
    }
    Ok(pruned)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> Result<Response, ContractError> {
    pool_creation_reply(deps, env, msg)
}

pub fn pool_creation_reply(deps: DepsMut, env: Env, msg: Reply) -> Result<Response, ContractError> {
    let (pool_id, step) = decode_reply_id(msg.id);
    match step {
        FINALIZE_POOL => finalize_pool(deps, env, msg, pool_id),
        _ => Err(ContractError::UnknownReplyId { id: msg.id }),
    }
}

/// F-1 — register/rotate the multi-hop router address (admin-only). Stored
/// so pools can exempt the router from the SimpleSwap belief_price
/// requirement. Not fund-touching; a wrong value only makes the real
/// router's null-belief swaps fail until corrected, so this is a direct
/// admin op rather than a 48h-timelocked config change.
pub fn execute_set_router(
    deps: DepsMut,
    info: MessageInfo,
    router: String,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;
    let router_addr = deps.api.addr_validate(&router)?;
    crate::state::ROUTER_ADDRESS.save(deps.storage, &router_addr)?;
    Ok(Response::new()
        .add_attribute("action", "set_router")
        .add_attribute("router", router_addr))
}

/// Admin gate used by every admin-only handler in this module's submodules.
/// Loads the factory config and rejects with [`ContractError::Unauthorized`]
/// if `info.sender` does not match `factory_admin_address`.
pub fn ensure_admin(deps: Deps, info: &MessageInfo) -> Result<(), ContractError> {
    let config = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    if info.sender != config.factory_admin_address {
        return Err(ContractError::Unauthorized {});
    }
    Ok(())
}
