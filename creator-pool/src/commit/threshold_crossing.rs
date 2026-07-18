//! Threshold-crossing commit handlers. Fire when a single commit carries
//! the pool over its `commit_amount_for_threshold_usd` target.
//!
//! Phase-2 responsibilities (in order):
//! 1. Split the incoming commit into a threshold portion (up to the
//!    remaining target) and an excess portion.
//! 2. Credit the threshold portion to `COMMIT_LEDGER` +
//!    `USD_RAISED_FROM_COMMIT` / `NATIVE_RAISED_FROM_COMMIT`, then run the
//!    payout: mint the splits, schedule the distribution airdrop, and emit
//!    the `MsgCreateBalancerPool` SubMsg that seeds the NATIVE pool.
//! 3. REFUND the entire post-fee bluechip excess to the crosser via
//!    `BankMsg::Send` — there is no inline swap anymore (the native pool
//!    doesn't exist yet within this tx; third-party trading happens on the
//!    native pool once seeded).
//! 4. Update commit analytics and clear `THRESHOLD_PROCESSING`.

use cosmwasm_std::{Addr, Coin, CosmosMsg, Decimal, DepsMut, Env, Response, Uint128};

use crate::asset::{get_native_denom, TokenInfo};
use crate::error::ContractError;
use crate::generic_helpers::{
    get_bank_transfer_to_msg, trigger_threshold_payout, update_commit_info,
};
use crate::msg::CommitFeeInfo;
use crate::state::{
    CommitLimitInfo, PoolAnalytics, PoolInfo, PoolSpecs, ThresholdPayoutAmounts,
    IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, THRESHOLD_PROCESSING, USD_RAISED_FROM_COMMIT,
};
use crate::swap_helper::usd_to_native_at_rate;

use super::commit_base_attributes;

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_threshold_crossing_with_excess(
    deps: &mut DepsMut,
    env: Env,
    sender: Addr,
    asset: &TokenInfo,
    amount: Uint128,
    amount_after_fees: Uint128,
    _commit_value: Uint128,
    value_to_threshold: Uint128,
    usd_rate: Uint128,
    pool_specs: &PoolSpecs,
    pool_info: &PoolInfo,
    commit_config: &CommitLimitInfo,
    threshold_payout: &ThresholdPayoutAmounts,
    fee_info: &CommitFeeInfo,
    bluechip_wallet: &Addr,
    // Live GAMM-fee context from the dispatcher's CommitContext query —
    // threaded into `trigger_threshold_payout` (see its docs).
    fee_cfg: Option<&Coin>,
    pricing_pool_id: u64,
    usd_quote_denom: &str,
    mut messages: Vec<CosmosMsg>,
    _belief_price: Option<Decimal>,
    _max_spread: Option<Decimal>,
    analytics: &mut PoolAnalytics,
) -> Result<Response, ContractError> {
    // Defensive entry gate: refuse to re-cross.
    if IS_THRESHOLD_HIT.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::StuckThresholdProcessing);
    }

    // The threshold gap is USD-denominated; convert it back to native at
    // EXACTLY the rate captured at commit entry so the split is
    // arithmetically consistent with the valuation.
    let bluechip_to_threshold = usd_to_native_at_rate(value_to_threshold, usd_rate)?;
    let _bluechip_excess = asset.amount.checked_sub(bluechip_to_threshold)?;

    let threshold_portion_after_fees = if amount.is_zero() {
        Uint128::zero()
    } else {
        amount_after_fees.multiply_ratio(bluechip_to_threshold, amount)
    };
    // The entire post-fee excess is refunded to the crosser (no inline
    // swap). Third-party trades happen on the native pool after seeding.
    let effective_bluechip_excess = amount_after_fees.checked_sub(threshold_portion_after_fees)?;

    // Update commit ledger with only the threshold portion, bumping the
    // O(1) distinct-committer counter if the crosser is new (FIX B). The
    // counter is read by `trigger_threshold_payout` below to size the
    // initial `distributions_remaining`, so it MUST reflect the crosser
    // before the payout runs — hence the insert-and-count happens here.
    super::record_committer(deps.storage, &sender, value_to_threshold)?;
    USD_RAISED_FROM_COMMIT.save(deps.storage, &commit_config.commit_amount_for_threshold_usd)?;
    // NATIVE_RAISED_FROM_COMMIT stores the NET bluechip entering the pool
    // for the threshold portion. The excess is refunded, not seeded.
    NATIVE_RAISED_FROM_COMMIT.update::<_, ContractError>(deps.storage, |r| {
        Ok(r.checked_add(threshold_portion_after_fees)?)
    })?;

    // Run the payout: mints + distribution setup + the MsgCreateBalancerPool
    // SubMsg that seeds the native pool. IS_THRESHOLD_HIT is flipped inside.
    let payout_msgs = trigger_threshold_payout(
        deps.storage,
        &deps.querier,
        pool_info,
        commit_config,
        threshold_payout,
        fee_info,
        bluechip_wallet,
        pool_specs.lp_fee,
        usd_rate,
        fee_cfg,
        pricing_pool_id,
        usd_quote_denom,
        &env,
    )?;
    messages.extend(payout_msgs.other_msgs);

    // Refund the entire post-fee excess to the crosser.
    let mut refunded_excess = Uint128::zero();
    if !effective_bluechip_excess.is_zero() {
        let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;
        messages.push(get_bank_transfer_to_msg(
            &sender,
            &bluechip_denom,
            effective_bluechip_excess,
        )?);
        refunded_excess = effective_bluechip_excess;
    }

    // Commit-info records the threshold portion only (the excess was
    // refunded). Fees on the whole commit were already transferred out by
    // the dispatcher's `build_fee_messages`.
    update_commit_info(
        deps.storage,
        &sender,
        &pool_info.pool_info.contract_addr,
        bluechip_to_threshold,
        value_to_threshold,
        env.block.time,
    )?;

    THRESHOLD_PROCESSING.save(deps.storage, &false)?;

    let base = commit_base_attributes(
        "threshold_crossing",
        &sender,
        &pool_info.pool_info.contract_addr,
        analytics.total_commit_count,
        &env,
    );
    // Order matters: acquire a cross-denom creation fee FIRST (fee_swap,
    // when the chain's fee is not native-denominated), create the native
    // pool NEXT (the gamm module charges the fee from the pool's balance),
    // THEN remit any creation-fee reserve leftover to the bluechip wallet
    // (FIX E).
    let mut response = Response::new().add_messages(messages);
    if let Some(swap) = payout_msgs.fee_swap {
        response = response.add_message(swap);
    }
    let mut response = response.add_submessage(payout_msgs.create_pool);
    if let Some(remit) = payout_msgs.reserve_remit {
        response = response.add_message(remit);
    }
    Ok(response
        .add_submessage(payout_msgs.factory_notify)
        .add_attributes(base)
        .add_attribute("total_amount_bluechip", asset.amount.to_string())
        .add_attribute(
            "threshold_amount_bluechip",
            bluechip_to_threshold.to_string(),
        )
        .add_attribute("bluechip_excess_refunded", refunded_excess.to_string()))
}

/// Threshold-hit-exact handler — commit hits the target precisely (no
/// excess to refund). Sister to
/// [`process_threshold_crossing_with_excess`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_threshold_hit_exact(
    deps: &mut DepsMut,
    env: Env,
    sender: Addr,
    asset: &TokenInfo,
    amount_after_fees: Uint128,
    commit_value: Uint128,
    new_total: Uint128,
    pool_specs: &PoolSpecs,
    pool_info: &PoolInfo,
    commit_config: &CommitLimitInfo,
    threshold_payout: &ThresholdPayoutAmounts,
    fee_info: &CommitFeeInfo,
    bluechip_wallet: &Addr,
    // Rate + live GAMM-fee context from the dispatcher's CommitContext
    // query — threaded into `trigger_threshold_payout` (see its docs).
    usd_rate: Uint128,
    fee_cfg: Option<&Coin>,
    pricing_pool_id: u64,
    usd_quote_denom: &str,
    mut messages: Vec<CosmosMsg>,
    analytics: &PoolAnalytics,
) -> Result<Response, ContractError> {
    if IS_THRESHOLD_HIT.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::StuckThresholdProcessing);
    }

    // Insert the crosser into the ledger + bump COMMITTER_COUNT if new
    // (FIX B) before `trigger_threshold_payout` reads it below.
    super::record_committer(deps.storage, &sender, commit_value)?;
    let final_raised = new_total.min(commit_config.commit_amount_for_threshold_usd);
    USD_RAISED_FROM_COMMIT.save(deps.storage, &final_raised)?;
    NATIVE_RAISED_FROM_COMMIT
        .update::<_, ContractError>(deps.storage, |r| Ok(r.checked_add(amount_after_fees)?))?;

    let payout = trigger_threshold_payout(
        deps.storage,
        &deps.querier,
        pool_info,
        commit_config,
        threshold_payout,
        fee_info,
        bluechip_wallet,
        pool_specs.lp_fee,
        usd_rate,
        fee_cfg,
        pricing_pool_id,
        usd_quote_denom,
        &env,
    )?;
    messages.extend(payout.other_msgs);
    update_commit_info(
        deps.storage,
        &sender,
        &pool_info.pool_info.contract_addr,
        asset.amount,
        commit_value,
        env.block.time,
    )?;
    THRESHOLD_PROCESSING.save(deps.storage, &false)?;

    let base = commit_base_attributes(
        "threshold_hit_exact",
        &sender,
        &pool_info.pool_info.contract_addr,
        analytics.total_commit_count,
        &env,
    );
    // Order matters: acquire a cross-denom creation fee FIRST (fee_swap),
    // create the native pool NEXT (the gamm module charges the fee), THEN
    // remit any creation-fee reserve leftover (FIX E).
    let mut response = Response::new().add_messages(messages);
    if let Some(swap) = payout.fee_swap {
        response = response.add_message(swap);
    }
    let mut response = response.add_submessage(payout.create_pool);
    if let Some(remit) = payout.reserve_remit {
        response = response.add_message(remit);
    }
    Ok(response
        .add_submessage(payout.factory_notify)
        .add_attributes(base)
        .add_attribute("commit_amount_bluechip", asset.amount.to_string())
        .add_attribute("total_raised_after", new_total.to_string()))
}
