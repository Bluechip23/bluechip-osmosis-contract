//! Pre-threshold commit handler.
//!
//! Runs while the pool is still accumulating USD towards its
//! `commit_amount_for_threshold_usd` target. Each call appends to
//! `COMMIT_LEDGER` for the sender and bumps the cumulative
//! `USD_RAISED_FROM_COMMIT` / `NATIVE_RAISED_FROM_COMMIT` totals.
//! Threshold-crossing commits are routed elsewhere — see
//! `super::threshold_crossing` and `super::execute_commit_logic`.

use cosmwasm_std::{Addr, CosmosMsg, DepsMut, Env, Response, Uint128};

use crate::asset::TokenInfo;
use crate::error::ContractError;
use crate::generic_helpers::update_commit_info;
use crate::state::{PoolAnalytics, NATIVE_RAISED_FROM_COMMIT, USD_RAISED_FROM_COMMIT};

use super::commit_base_attributes;

#[allow(clippy::too_many_arguments)]
pub(super) fn process_pre_threshold_commit(
    deps: &mut DepsMut,
    env: Env,
    sender: Addr,
    asset: &TokenInfo,
    commit_value: Uint128,
    net_bluechip: Uint128,
    new_usd_total: Uint128,
    messages: Vec<CosmosMsg>,
    pool_contract_addr: &Addr,
    analytics: &mut PoolAnalytics,
) -> Result<Response, ContractError> {
    // Append to the ledger and bump the O(1) distinct-committer counter
    // iff `sender` is new (FIX B). `record_committer` does the
    // has()-before-update check so repeat committers never double-count.
    super::record_committer(deps.storage, &sender, commit_value)?;
    // `new_usd_total` is the dispatcher's already-computed
    // `USD_RAISED_FROM_COMMIT + commit_value` (overflow-checked there,
    // and the routing into this handler depends on it), so save it
    // directly instead of re-reading the item for an identical add.
    //
    // `NATIVE_RAISED_FROM_COMMIT` stores the *net* bluechip that
    // entered the contract's bank balance from this commit. Storing
    // net directly — rather than the gross `asset.amount` with
    // `trigger_threshold_payout` recovering the net via a
    // `gross * (1 - fee_rate)` floor — avoids a second flooring step
    // whose rounding wouldn't exactly match the per-commit fee floor
    // (stranding up to ~2 units per commit in the contract forever)
    // and makes the seed math exact:
    // `pools_bluechip_seed = NATIVE_RAISED`.
    USD_RAISED_FROM_COMMIT.save(deps.storage, &new_usd_total)?;
    let total_raised = new_usd_total;
    let total_bluechip_raised = NATIVE_RAISED_FROM_COMMIT
        .update::<_, ContractError>(deps.storage, |r| Ok(r.checked_add(net_bluechip)?))?;

    update_commit_info(
        deps.storage,
        &sender,
        pool_contract_addr,
        asset.amount,
        commit_value,
        env.block.time,
    )?;

    // Analytics counter is incremented and persisted by the dispatcher
    // (`commit::execute_commit_logic`); this handler only reads the
    // already-bumped `total_commit_count` for response attributes.
    let base = commit_base_attributes(
        "funding",
        &sender,
        pool_contract_addr,
        analytics.total_commit_count,
        &env,
    );
    Ok(Response::new()
        .add_messages(messages)
        .add_attributes(base)
        .add_attribute("commit_amount_bluechip", asset.amount.to_string())
        .add_attribute("total_raised_after", total_raised.to_string())
        .add_attribute(
            "total_bluechip_raised_after",
            total_bluechip_raised.to_string(),
        ))
}
