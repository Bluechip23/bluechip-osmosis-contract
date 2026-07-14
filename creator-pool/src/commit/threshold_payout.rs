//! Threshold-crossing payout orchestration.
//!
//! Runs once per pool when a commit crosses the
//! `commit_amount_for_threshold_usd` target. It:
//! - Mints the four creator-token splits (`creator_reward_amount` →
//!   creator wallet, `bluechip_reward_amount` → bluechip wallet,
//!   `pool_seed_amount` → the POOL CONTRACT, `commit_return_amount`
//!   funds the post-threshold committer airdrop) via TokenFactory MsgMint.
//! - Schedules the post-threshold distribution batch loop
//!   (DISTRIBUTION_STATE), unchanged — it is independent of the pool.
//! - **Seeds a NATIVE Osmosis GAMM balancer pool** with the raised
//!   bluechip (capped at `max_bluechip_lock_per_pool`) and the pool-seed
//!   creator tokens. This replaces the old internal reserve seeding. The
//!   `MsgCreateBalancerPool` rides back on the crossing Response as a
//!   `SubMsg::reply_on_success(_, REPLY_ID_CREATE_POOL)`; the reply parses
//!   the new `pool_id` and stores it. The pool holds the resulting
//!   `gamm/pool/{id}` LP shares permanently.
//! - When the raised bluechip exceeds the cap, records a time-locked
//!   creator entitlement to a proportional slice of the seed LP shares
//!   (`CREATOR_EXCESS_POSITION`), claimed later via
//!   `ClaimCreatorExcessLiquidity`.
//! - Flips `IS_THRESHOLD_HIT` (the load-bearing no-double-mint gate) and
//!   snapshots `THRESHOLD_CROSSED_AT`.
//!
//! The factory-notify SubMsg is held aside and attached as a
//! `reply_on_error` SubMsg so a factory-side failure does NOT revert the
//! pool's threshold-crossing state (retryable via `RetryFactoryNotify`).

use cosmwasm_std::{
    to_json_binary, Addr, Coin, CosmosMsg, Decimal, Env, Order, StdError, Storage, SubMsg, Uint128,
    WasmMsg,
};

use crate::asset::get_native_denom;
use crate::error::ContractError;
use crate::msg::CommitFeeInfo;
use crate::state::{
    CommitLimitInfo, CreatorExcessLiquidity, DistributionState, PoolInfo, ThresholdPayoutAmounts,
    COMMIT_LEDGER, CREATOR_EXCESS_POSITION, DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
    DEFAULT_MAX_GAS_PER_TX, DISTRIBUTION_STATE, IS_THRESHOLD_HIT, REPLY_ID_CREATE_POOL,
    SECONDS_PER_DAY, THRESHOLD_PAYOUT_BLUECHIP_BASE_UNITS, THRESHOLD_PAYOUT_COMMIT_RETURN_BASE_UNITS,
    THRESHOLD_PAYOUT_CREATOR_BASE_UNITS, THRESHOLD_PAYOUT_POOL_BASE_UNITS,
    THRESHOLD_PAYOUT_TOTAL_BASE_UNITS,
};
use pool_core::osmosis_msgs::create_balancer_pool_msg;

/// Validate that the four threshold-payout components match the canonical
/// per-pool split (325B + 25B + 350B + 500B = 1.2T base units).
pub fn validate_pool_threshold_payments(
    params: &ThresholdPayoutAmounts,
) -> Result<(), ContractError> {
    if params.creator_reward_amount != Uint128::new(THRESHOLD_PAYOUT_CREATOR_BASE_UNITS) {
        return Err(ContractError::InvalidThresholdParams {
            msg: format!(
                "Creator amount must be {}",
                THRESHOLD_PAYOUT_CREATOR_BASE_UNITS
            ),
        });
    }
    if params.bluechip_reward_amount != Uint128::new(THRESHOLD_PAYOUT_BLUECHIP_BASE_UNITS) {
        return Err(ContractError::InvalidThresholdParams {
            msg: format!(
                "BlueChip amount must be {}",
                THRESHOLD_PAYOUT_BLUECHIP_BASE_UNITS
            ),
        });
    }
    if params.pool_seed_amount != Uint128::new(THRESHOLD_PAYOUT_POOL_BASE_UNITS) {
        return Err(ContractError::InvalidThresholdParams {
            msg: format!("Pool amount must be {}", THRESHOLD_PAYOUT_POOL_BASE_UNITS),
        });
    }
    if params.commit_return_amount != Uint128::new(THRESHOLD_PAYOUT_COMMIT_RETURN_BASE_UNITS) {
        return Err(ContractError::InvalidThresholdParams {
            msg: format!(
                "Commit amount must be {}",
                THRESHOLD_PAYOUT_COMMIT_RETURN_BASE_UNITS
            ),
        });
    }

    let total = params
        .creator_reward_amount
        .checked_add(params.bluechip_reward_amount)?
        .checked_add(params.pool_seed_amount)?
        .checked_add(params.commit_return_amount)?;
    if total != Uint128::new(THRESHOLD_PAYOUT_TOTAL_BASE_UNITS) {
        return Err(ContractError::InvalidThresholdParams {
            msg: format!(
                "Total must equal {} (got {})",
                THRESHOLD_PAYOUT_TOTAL_BASE_UNITS, total
            ),
        });
    }

    Ok(())
}

/// Output of `trigger_threshold_payout`.
///
/// - `factory_notify`: `reply_on_error` SubMsg — a failure there must NOT
///   revert the pool-side threshold-crossing state.
/// - `create_pool`: `reply_on_success` SubMsg carrying `MsgCreateBalancerPool`
///   (REPLY_ID_CREATE_POOL). Executes AFTER the mints so the pool holds its
///   seed coins when the balancer pool is created.
/// - `other_msgs`: the plain mint CosmosMsgs (creator/bluechip/pool-seed).
#[derive(Debug)]
pub struct ThresholdPayoutMsgs {
    pub factory_notify: SubMsg,
    pub create_pool: SubMsg,
    pub other_msgs: Vec<CosmosMsg>,
}

#[allow(clippy::too_many_arguments)]
pub fn trigger_threshold_payout(
    storage: &mut dyn Storage,
    pool_info: &PoolInfo,
    commit_config: &CommitLimitInfo,
    payout: &ThresholdPayoutAmounts,
    fee_info: &CommitFeeInfo,
    // Live-resolved bluechip protocol-wallet (returned by the factory's
    // `CommitContext` query at the entry point). Recipient of the
    // 25k-base-unit bluechip-share mint.
    bluechip_wallet: &Addr,
    // LP fee (`PoolSpecs.lp_fee`) reused as the native GAMM pool's swap_fee.
    lp_fee: Decimal,
    env: &Env,
) -> Result<ThresholdPayoutMsgs, ContractError> {
    // No-double-mint invariant — STRUCTURALLY enforced here. This is the
    // single load-bearing path that mints the 1.2T splits and seeds the
    // native pool; running it twice would re-mint and re-seed.
    if IS_THRESHOLD_HIT.may_load(storage)?.unwrap_or(false) {
        return Err(ContractError::StuckThresholdProcessing);
    }

    // Factory notification goes out as a `reply_on_error` SubMsg. On
    // failure the pool's `reply` sets PENDING_FACTORY_NOTIFY=true.
    let factory_notify = SubMsg::reply_on_error(
        CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: pool_info.factory_addr.to_string(),
            msg: to_json_binary(
                &pool_factory_interfaces::FactoryExecuteMsg::NotifyThresholdCrossed {
                    pool_id: pool_info.pool_id,
                    crossed_at: Some(env.block.time),
                },
            )?,
            funds: vec![],
        }),
        crate::state::REPLY_ID_FACTORY_NOTIFY_INITIAL,
    );

    let mut other_msgs: Vec<CosmosMsg> = Vec::new();

    // Runtime sanity check that the four payout components add up.
    let total = payout
        .creator_reward_amount
        .checked_add(payout.bluechip_reward_amount)?
        .checked_add(payout.pool_seed_amount)?
        .checked_add(payout.commit_return_amount)?;

    if total != Uint128::new(THRESHOLD_PAYOUT_TOTAL_BASE_UNITS) {
        return Err(ContractError::ThresholdPayoutCorruption);
    }

    // Mint the three up-front splits (the commit-return split is minted
    // per-committer during distribution, from the pool as denom admin).
    other_msgs.push(mint_tokens(
        &pool_info.pool_info.contract_addr,
        &pool_info.token_denom,
        &fee_info.creator_wallet_address,
        payout.creator_reward_amount,
    ));
    other_msgs.push(mint_tokens(
        &pool_info.pool_info.contract_addr,
        &pool_info.token_denom,
        bluechip_wallet,
        payout.bluechip_reward_amount,
    ));
    // pool_seed_amount is minted to the POOL CONTRACT so it holds the
    // creator side when MsgCreateBalancerPool executes.
    other_msgs.push(mint_tokens(
        &pool_info.pool_info.contract_addr,
        &pool_info.token_denom,
        &env.contract.address,
        payout.pool_seed_amount,
    ));

    // Post-threshold committer distribution setup (unchanged — independent
    // of the pool venue).
    let committer_count_usize = COMMIT_LEDGER
        .keys(storage, None, None, Order::Ascending)
        .count();
    let committer_count = u32::try_from(committer_count_usize).unwrap_or(u32::MAX);

    if committer_count > 0 {
        let dist_state = DistributionState {
            is_distributing: true,
            total_to_distribute: payout.commit_return_amount,
            total_committed_usd: commit_config.commit_amount_for_threshold_usd,
            last_processed_key: None,
            distributions_remaining: committer_count,
            estimated_gas_per_distribution: DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
            max_gas_per_tx: DEFAULT_MAX_GAS_PER_TX,
            last_successful_batch_size: None,
            consecutive_failures: 0,
            started_at: env.block.time,
            last_updated: env.block.time,
            distributed_so_far: cosmwasm_std::Uint128::zero(),
        };
        DISTRIBUTION_STATE.save(storage, &dist_state)?;
    }

    // NATIVE_RAISED_FROM_COMMIT is stored net-of-fees; read directly.
    let pools_bluechip_seed = crate::state::NATIVE_RAISED_FROM_COMMIT.load(storage)?;

    // Compute the coins seeding the native pool. The bluechip side is
    // capped at `max_bluechip_lock_per_pool`; the creator side is reduced
    // proportionally so the seeded ratio matches the retired internal-AMM
    // reserve seeding. The over-raise is compensated to the creator as a
    // time-locked slice of the seed LP shares (decision 5).
    let (seed_osmo, seed_creator) = if pools_bluechip_seed
        > commit_config.max_bluechip_lock_per_pool
    {
        let excess_bluechip = pools_bluechip_seed
            .checked_sub(commit_config.max_bluechip_lock_per_pool)
            .map_err(StdError::overflow)?;

        let excess_creator_tokens = payout
            .pool_seed_amount
            .multiply_ratio(excess_bluechip, pools_bluechip_seed);

        CREATOR_EXCESS_POSITION.save(
            storage,
            &CreatorExcessLiquidity {
                creator: fee_info.creator_wallet_address.clone(),
                excess_bluechip,
                total_seeded_bluechip: pools_bluechip_seed,
                unlock_time: env.block.time.plus_seconds(
                    commit_config.creator_excess_liquidity_lock_days * SECONDS_PER_DAY,
                ),
            },
        )?;

        let seed_creator = payout
            .pool_seed_amount
            .checked_sub(excess_creator_tokens)
            .map_err(StdError::overflow)?;
        (commit_config.max_bluechip_lock_per_pool, seed_creator)
    } else {
        (pools_bluechip_seed, payout.pool_seed_amount)
    };

    // Build the MsgCreateBalancerPool SubMsg. asset_infos[0] is the
    // bluechip Native side, [1] the creator TokenFactory side.
    let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;
    let coin_osmo = Coin {
        denom: bluechip_denom,
        amount: seed_osmo,
    };
    let coin_creator = Coin {
        denom: pool_info.token_denom.clone(),
        amount: seed_creator,
    };
    let create_pool = SubMsg::reply_on_success(
        create_balancer_pool_msg(
            &pool_info.pool_info.contract_addr,
            &coin_osmo,
            &coin_creator,
            lp_fee,
        ),
        REPLY_ID_CREATE_POOL,
    );

    // Set IS_THRESHOLD_HIT only after all mint + seed work is scheduled.
    IS_THRESHOLD_HIT.save(storage, &true)?;
    crate::state::THRESHOLD_CROSSED_AT.save(storage, &env.block.time)?;

    Ok(ThresholdPayoutMsgs {
        factory_notify,
        create_pool,
        other_msgs,
    })
}

/// Build a TokenFactory `MsgMint` that mints `amount` of the creator
/// token `denom` and credits `recipient`. `pool_addr` is the pool
/// contract — the denom admin — which is the required `sender`.
pub fn mint_tokens(pool_addr: &Addr, denom: &str, recipient: &Addr, amount: Uint128) -> CosmosMsg {
    pool_core::osmosis_msgs::mint_msg(pool_addr, denom, amount, recipient)
}
