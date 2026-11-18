//! Post-threshold distribution batch processing.
//!
//! Once a pool crosses its commit threshold, the commit ledger is paid
//! out to committers in batches. Each call to
//! `execute_continue_distribution` processes the next slice and
//! advances the cursor in `DISTRIBUTION_STATE` until the pool is fully
//! distributed. The call is permissionless and unpaid; the protocol's
//! distribution keeper runs it as housekeeping (see keepers/).

use cosmwasm_std::{DepsMut, Env, MessageInfo, Response, SubMsg};

use crate::admin::ensure_not_drained;
use crate::error::ContractError;
use crate::generic_helpers::process_distribution_batch;
use crate::state::{
    CONTINUE_DISTRIBUTION_RATE_LIMIT_SECONDS, DISTRIBUTION_STATE, LAST_CONTINUE_DISTRIBUTION_AT,
    POOL_INFO, POOL_PAUSED,
};

/// Contract-routed trading (`SimpleSwap` and post-threshold `Commit`) stays
/// closed until every committer's ledger row has been processed, so no one
/// trades through this contract while others are still waiting on their
/// tokens. `DISTRIBUTION_STATE` is written at crossing and removed by the
/// final batch, so its presence is the "still distributing" signal. (After
/// an emergency drain it is left in place, which keeps this venue closed.)
///
/// This cannot gate trades made directly against the native Osmosis pool,
/// which exists from the crossing transaction onward.
pub fn ensure_distribution_complete(
    storage: &dyn cosmwasm_std::Storage,
) -> Result<(), ContractError> {
    if DISTRIBUTION_STATE.may_load(storage)?.is_some()
        || crate::state::PENDING_POOL_SEED.may_load(storage)?.is_some()
    {
        return Err(ContractError::DistributionInProgress {});
    }
    Ok(())
}

/// Final step of the launch: once the committer ledger is empty, mint the
/// creator / protocol / pool-seed tokens and create the native pool. Runs
/// as the `ContinueDistribution` call that follows the last batch.
fn execute_seed_pool(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    let fee_info = crate::state::COMMITFEEINFO.load(deps.storage)?;
    let payout = crate::state::THRESHOLD_PAYOUT_AMOUNTS.load(deps.storage)?;
    let specs = crate::state::POOL_SPECS.load(deps.storage)?;

    // Live protocol wallet, falling back to the create-time snapshot so a
    // factory outage cannot block the pool from opening.
    let bluechip_wallet = match deps
        .querier
        .query_wasm_smart::<pool_factory_interfaces::BluechipWalletResponse>(
            pool_info.factory_addr.to_string(),
            &pool_factory_interfaces::FactoryQueryEnvelope::PoolFactoryQuery(
                pool_factory_interfaces::FactoryQueryMsg::BluechipWalletAddress {},
            ),
        ) {
        Ok(resp) => resp.address,
        Err(_) => fee_info.bluechip_wallet_address.clone(),
    };

    let (mints, create_pool) = crate::commit::threshold_payout::build_pool_seed(
        deps.storage,
        &deps.querier,
        &env,
        &pool_info,
        &fee_info,
        &payout,
        &bluechip_wallet,
        specs.lp_fee,
    )?;
    // The distribution is over; clearing its state (if a recovery path has
    // not already) is what reopens contract-routed trading.
    DISTRIBUTION_STATE.remove(deps.storage);

    Ok(Response::new()
        .add_messages(mints)
        .add_submessage(create_pool)
        .add_attribute("action", "continue_distribution")
        .add_attribute("phase", "pool_seeded")
        .add_attribute("caller", info.sender.to_string())
        .add_attribute("processed_count", "0")
        .add_attribute("remaining_after", "0")
        .add_attribute("distribution_complete", "true")
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

pub fn execute_continue_distribution(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    // Defense in depth: emergency_withdraw flips is_distributing=false on
    // drain (admin.rs::execute_emergency_withdraw), so the load+check below
    // already rejects calls on drained pools — but reading EMERGENCY_DRAINED
    // up front fails early with the canonical error and avoids the keeper
    // ever issuing a tx against a drained pool.
    ensure_not_drained(deps.storage)?;

    // Honor admin pause. Distribution is permissionless (any keeper may
    // call it), but it mints creator tokens to committers — exactly the
    // kind of state-mutating activity an admin pause is meant to halt
    // while investigating suspicious behavior. Every other liquidity-
    // touching path goes through `check_pool_writable`, which checks
    // both pause AND drain; this brings distribution under the same
    // uniform halt semantics. Pause is reversible by the factory, so
    // legitimate distribution resumes once the admin clears the pause.
    if POOL_PAUSED.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::PoolPausedLowLiquidity {});
    }

    // Per-address rate limit. Same-block spam from a single keeper
    // (or two competing keepers) wastes gas on no-op tx after the
    // ledger is empty / cursor is past the end. Reject if this address
    // called within the last CONTINUE_DISTRIBUTION_RATE_LIMIT_SECONDS.
    // Stamp the new timestamp before any further work — a successful
    // tx records the call; a tx that errors below atomically reverts
    // the stamp along with everything else.
    let now = env.block.time.seconds();
    if let Some(prev) = LAST_CONTINUE_DISTRIBUTION_AT.may_load(deps.storage, &info.sender)? {
        let earliest_next = prev.saturating_add(CONTINUE_DISTRIBUTION_RATE_LIMIT_SECONDS);
        if now < earliest_next {
            return Err(ContractError::ContinueDistributionRateLimited {
                earliest_next,
                last_call: prev,
                cooldown_seconds: CONTINUE_DISTRIBUTION_RATE_LIMIT_SECONDS,
            });
        }
    }
    LAST_CONTINUE_DISTRIBUTION_AT.save(deps.storage, &info.sender, &now)?;

    // Every committer has been processed and the pool is still unseeded:
    // this call opens the pool instead of running a batch.
    if crate::state::PENDING_POOL_SEED
        .may_load(deps.storage)?
        .is_some()
        && crate::state::COMMIT_LEDGER
            .keys(deps.storage, None, None, cosmwasm_std::Order::Ascending)
            .next()
            .is_none()
    {
        return execute_seed_pool(deps, env, info);
    }

    let dist_state = DISTRIBUTION_STATE.load(deps.storage)?;
    if !dist_state.is_distributing {
        return Err(ContractError::NothingToRecover {});
    }

    let pool_info = POOL_INFO.load(deps.storage)?;

    // process_distribution_batch returns a `Vec<SubMsg>` — each per-user
    // mint is wrapped in `reply_always` so a single failing recipient
    // cannot revert the batch. Failures land in `FAILED_MINTS`
    // via the contract's reply handler and are claimable later through
    // `ClaimFailedDistribution`.
    let (mint_submsgs, processed_count): (Vec<SubMsg>, u32) =
        process_distribution_batch(deps.storage, &pool_info, &env)?;

    // process_distribution_batch may have either removed the state
    // entirely (genuine completion) or flipped is_distributing=false
    // (recovery path after repeated failures). Treat both as "stop
    // calling this pool" from the keeper's perspective.
    let (remaining_after, is_complete) = match DISTRIBUTION_STATE.may_load(deps.storage)? {
        None => (0u32, true),
        Some(d) => (d.distributions_remaining, !d.is_distributing),
    };

    Ok(Response::new()
        .add_submessages(mint_submsgs)
        .add_attribute("action", "continue_distribution")
        .add_attribute("caller", info.sender.to_string())
        .add_attribute("processed_count", processed_count.to_string())
        .add_attribute("bounty_paid", (processed_count > 0).to_string())
        .add_attribute(
            "remaining_before",
            dist_state.distributions_remaining.to_string(),
        )
        .add_attribute("remaining_after", remaining_after.to_string())
        .add_attribute("distribution_complete", is_complete.to_string())
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}
