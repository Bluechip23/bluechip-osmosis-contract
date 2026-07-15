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
//!   creator entitlement to the RAW excess coins — the over-cap bluechip
//!   and the proportional creator tokens (`CREATOR_EXCESS_POSITION`),
//!   which REMAIN in the contract's bank balance and are claimed later
//!   via `ClaimCreatorExcessLiquidity` (FIX C).
//! - Flips `IS_THRESHOLD_HIT` (the load-bearing no-double-mint gate) and
//!   snapshots `THRESHOLD_CROSSED_AT`.
//!
//! The factory-notify SubMsg is held aside and attached as a
//! `reply_on_error` SubMsg so a factory-side failure does NOT revert the
//! pool's threshold-crossing state (retryable via `RetryFactoryNotify`).

use cosmwasm_std::{
    to_json_binary, Addr, BankMsg, Coin, CosmosMsg, Decimal, Env, StdError, Storage, SubMsg,
    Uint128, WasmMsg,
};

use crate::asset::get_native_denom;
use crate::error::ContractError;
use crate::msg::CommitFeeInfo;
use crate::state::{
    CommitLimitInfo, CreatorExcessLiquidity, DistributionState, PoolInfo, ThresholdPayoutAmounts,
    COMMITTER_COUNT, CREATOR_EXCESS_POSITION, DEFAULT_ESTIMATED_GAS_PER_DISTRIBUTION,
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
    /// FIX E — bank-send of any creation-fee reserve LEFTOVER
    /// (`reserved - creation_fee`) back to the bluechip wallet. `Some` only
    /// when the retained reserve exceeded the actual gamm creation fee (the
    /// common case, since the reserve fills to the target during funding).
    /// The caller MUST emit this AFTER `create_pool` so the gamm module has
    /// already charged the fee from the pool's OSMO balance; remitting the
    /// surplus earlier would still be arithmetically safe (only the strict
    /// surplus is remitted) but sequencing it post-creation keeps the intent
    /// obvious and leaves the full reserve available during pool creation.
    pub reserve_remit: Option<CosmosMsg>,
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
    // of the pool venue). Distinct-committer count is read O(1) from the
    // incrementally-maintained `COMMITTER_COUNT` (FIX B) rather than the
    // old unbounded `COMMIT_LEDGER.keys(..).count()` scan. At crossing the
    // ledger is full (nothing distributed yet), so the counter equals the
    // ledger size exactly — the crossing handler recorded the crosser
    // before calling this.
    let committer_count = COMMITTER_COUNT.may_load(storage)?.unwrap_or(0);

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

    // FIX E — creation-fee reserve context. The pool's ACTUAL OSMO bank
    // balance at this point is `pools_bluechip_seed + reserved` (net commits
    // plus the bluechip fee retained toward the gamm creation fee). When
    // `MsgCreateBalancerPool` runs, the `x/gamm` module auto-charges
    // `creation_fee` from that balance IN ADDITION to the coins seeded into
    // the pool, so the pool must hold `>= seed_osmo + creation_fee` OSMO or
    // the create bricks the tx. `reserved` is the OSMO earmarked to cover it.
    let reserved = crate::state::BLUECHIP_FEE_RESERVED
        .may_load(storage)?
        .unwrap_or_default();
    let creation_fee = crate::state::CREATION_FEE_RESERVE_TARGET
        .may_load(storage)?
        .unwrap_or_default();

    // Compute the coins seeding the native pool. The bluechip side is
    // capped at `max_bluechip_lock_per_pool`; the creator side is reduced
    // proportionally so the seeded ratio matches the retired internal-AMM
    // reserve seeding.
    //
    // FIX C: on over-cap the excess is time-locked to the creator as RAW
    // coins (the original model), NOT as a slice of the pool's LP shares.
    // The pool is seeded with `max_bluechip_lock` OSMO +
    // `(pool_seed_amount - excess_creator_tokens)` creator tokens; the
    // earmarked excess coins REMAIN in the contract's bank balance:
    //   - excess OSMO: `pools_bluechip_seed - max_bluechip_lock` was
    //     received from commits and is simply not handed to the pool seed;
    //   - excess creator tokens: `pool_seed_amount` is minted to the
    //     contract IN FULL above, and only `seed_creator` of it is passed
    //     to `MsgCreateBalancerPool`, so `excess_creator_tokens` stays put.
    // Neither is sent anywhere at crossing; the creator claims the raw
    // coins after `unlock_time` via `ClaimCreatorExcessLiquidity`.
    let (base_seed_osmo, seed_creator) = if pools_bluechip_seed
        > commit_config.max_bluechip_lock_per_pool
    {
        let excess_bluechip = pools_bluechip_seed
            .checked_sub(commit_config.max_bluechip_lock_per_pool)
            .map_err(StdError::overflow)?;

        // Creator tokens earmarked in proportion to the over-raise:
        // `pool_seed_amount * excess_bluechip / pools_bluechip_seed`.
        let excess_creator_tokens = payout
            .pool_seed_amount
            .multiply_ratio(excess_bluechip, pools_bluechip_seed);

        CREATOR_EXCESS_POSITION.save(
            storage,
            &CreatorExcessLiquidity {
                creator: fee_info.creator_wallet_address.clone(),
                bluechip_amount: excess_bluechip,
                token_amount: excess_creator_tokens,
                unlock_time: env.block.time.plus_seconds(
                    commit_config.creator_excess_liquidity_lock_days * SECONDS_PER_DAY,
                ),
            },
        )?;

        // The pool is seeded with the NON-earmarked creator tokens so the
        // earmarked `excess_creator_tokens` stays in the contract for the
        // creator's later raw claim.
        let seed_creator = payout
            .pool_seed_amount
            .checked_sub(excess_creator_tokens)
            .map_err(StdError::overflow)?;
        (commit_config.max_bluechip_lock_per_pool, seed_creator)
    } else {
        (pools_bluechip_seed, payout.pool_seed_amount)
    };

    // FIX E — the SEED always yields the uncovered creation-fee shortfall,
    // so both the brick invariant and the creator earmark stay consistent.
    // The pool holds `pools_bluechip_seed + reserved` OSMO and the gamm
    // module auto-charges `creation_fee` ON TOP of the seeded coins. Whatever
    // the retained `reserved` does not cover — `shortfall = creation_fee -
    // reserved`, which is ZERO in the normal case where the reserve filled to
    // the fee — is subtracted from `seed_osmo` unconditionally:
    //   - `seed_osmo + creation_fee <= balance` always holds ⇒ no brick; and
    //   - in the over-cap case the earmarked `excess_bluechip` is left FULLY
    //     backed by the contract's post-crossing OSMO balance, so the
    //     creator's later raw claim can always be paid.
    // The protocol bears any shortfall via a smaller seed contribution — it
    // is NEVER drawn from the creator's earmarked excess. (Applying the
    // subtraction only when it would otherwise brick was subtly wrong: on a
    // large over-raise the fee could be silently covered by the excess OSMO,
    // over-recording the earmark and stranding the creator's claim.)
    let shortfall = creation_fee.saturating_sub(reserved);
    let seed_osmo = base_seed_osmo.saturating_sub(shortfall);

    // FIX G — snapshot the per-side liquidity ACTUALLY seeded (post the FIX-E
    // adjustment above) as the breaker's reference point. A later swap trips
    // the breaker if either live side falls below BREAKER_FLOOR_PERCENT% of
    // the amount recorded here.
    crate::state::SEED_LIQUIDITY.save(storage, &(seed_osmo, seed_creator))?;

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

    // FIX E — remit any creation-fee reserve LEFTOVER back to the bluechip
    // wallet. After the gamm module charges `creation_fee`, the retained
    // `reserved` has `reserved - creation_fee` to spare (zero in the
    // shortfall edge); that surplus is the protocol's fee income and is
    // returned to the live bluechip wallet. Emitted by the caller AFTER the
    // create-pool SubMsg (see `ThresholdPayoutMsgs::reserve_remit`).
    let leftover = reserved.saturating_sub(creation_fee);
    let reserve_remit = if leftover.is_zero() {
        None
    } else {
        Some(CosmosMsg::Bank(BankMsg::Send {
            to_address: bluechip_wallet.to_string(),
            amount: vec![Coin {
                denom: get_native_denom(&pool_info.pool_info.asset_infos)?,
                amount: leftover,
            }],
        }))
    };

    // FIX E — mark the reserve complete. The creation fee has now been
    // handled (covered at creation + any surplus remitted), so post-threshold
    // commits must NOT keep retaining bluechip toward the target. Pinning
    // `BLUECHIP_FEE_RESERVED` at the target makes `room == 0` in
    // `reserve_bluechip_fee` from here on, so every post-threshold commit
    // sends its full 1% bluechip fee to the wallet. (In the shortfall edge
    // `reserved < target`; pinning to the target still correctly stops
    // further retention — no funds move here, this is a bookkeeping flag.)
    crate::state::BLUECHIP_FEE_RESERVED.save(storage, &creation_fee)?;

    // Set IS_THRESHOLD_HIT only after all mint + seed work is scheduled.
    IS_THRESHOLD_HIT.save(storage, &true)?;
    crate::state::THRESHOLD_CROSSED_AT.save(storage, &env.block.time)?;

    Ok(ThresholdPayoutMsgs {
        factory_notify,
        create_pool,
        other_msgs,
        reserve_remit,
    })
}

/// Build a TokenFactory `MsgMint` that mints `amount` of the creator
/// token `denom` and credits `recipient`. `pool_addr` is the pool
/// contract — the denom admin — which is the required `sender`.
pub fn mint_tokens(pool_addr: &Addr, denom: &str, recipient: &Addr, amount: Uint128) -> CosmosMsg {
    pool_core::osmosis_msgs::mint_msg(pool_addr, denom, amount, recipient)
}
