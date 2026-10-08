//! Threshold-crossing commit handlers. Fire when a single commit carries
//! the pool over its `commit_amount_for_threshold_native` target.
//!
//! Responsibilities (in order):
//! 1. The dispatcher (`super::execute_commit_logic`) has already split the
//!    incoming commit into a threshold portion (exactly the remaining gap)
//!    and a gross excess, and charged the 1% + 5% commit fees on the
//!    THRESHOLD PORTION ONLY. The excess bears no commit fee; the one
//!    bounded exception is a creation-fee reserve top-up of at most the
//!    bluechip-fee rate of the excess, taken only while the reserve is
//!    still short (see `reserve_bluechip_fee`).
//! 2. Credit the threshold portion to `COMMIT_LEDGER` +
//!    `GROSS_NATIVE_COMMITTED` / `NATIVE_RAISED_FROM_COMMIT`, then run the
//!    payout: mint the splits, schedule the distribution airdrop, and emit
//!    the `MsgCreateBalancerPool` SubMsg that seeds the NATIVE pool.
//! 3. REFUND the gross excess (less any reserve retention) to the crosser
//!    via `BankMsg::Send` — there is no inline swap (the native pool
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
    IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, THRESHOLD_PROCESSING, GROSS_NATIVE_COMMITTED,
};

use super::commit_base_attributes;

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_threshold_crossing_with_excess(
    deps: &mut DepsMut,
    env: Env,
    sender: Addr,
    asset: &TokenInfo,
    // Net-of-fees THRESHOLD PORTION (the dispatcher charged the commit fees
    // on the gap only). This is exactly what enters the pool's bank balance
    // from this commit.
    threshold_portion_after_fees: Uint128,
    // Gross excess over the gap that goes back to the crosser.
    excess_refund: Uint128,
    // Part of the gross excess retained toward the gamm creation-fee
    // reserve (zero whenever the reserve was already full). For attributes
    // only — the dispatcher has already recorded it in
    // `BLUECHIP_FEE_RESERVED`.
    excess_retained: Uint128,
    value_to_threshold: Uint128,
    fee_swap_budget: Option<Uint128>,
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
    fee_quote_denom: &str,
    mut messages: Vec<CosmosMsg>,
    _belief_price: Option<Decimal>,
    _max_spread: Option<Decimal>,
    analytics: &mut PoolAnalytics,
) -> Result<Response, ContractError> {
    // Defensive entry gate: refuse to re-cross.
    if IS_THRESHOLD_HIT.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::StuckThresholdProcessing);
    }

    // The threshold gap is native-denominated — same units as the commit
    // itself — so the split needs no conversion: the portion of this commit
    // that fills the gap IS the gap. Sanity: gap + refund + retention must
    // reconstruct the gross commit exactly.
    let bluechip_to_threshold = value_to_threshold;
    let reconstructed = bluechip_to_threshold
        .checked_add(excess_refund)?
        .checked_add(excess_retained)?;
    if reconstructed != asset.amount {
        return Err(ContractError::ThresholdPayoutCorruption);
    }

    // Update commit ledger with only the threshold portion, bumping the
    // O(1) distinct-committer counter if the crosser is new. The
    // counter is read by `trigger_threshold_payout` below to size the
    // initial `distributions_remaining`, so it MUST reflect the crosser
    // before the payout runs — hence the insert-and-count happens here.
    super::record_committer(deps.storage, &sender, value_to_threshold)?;
    GROSS_NATIVE_COMMITTED.save(deps.storage, &commit_config.commit_amount_for_threshold_native)?;
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
        fee_swap_budget,
        fee_cfg,
        pricing_pool_id,
        fee_quote_denom,
        &env,
    )?;
    messages.extend(payout_msgs.other_msgs);

    // Refund the gross excess (less any reserve retention) to the crosser.
    if !excess_refund.is_zero() {
        let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;
        messages.push(get_bank_transfer_to_msg(
            &sender,
            &bluechip_denom,
            excess_refund,
        )?);
    }

    // Commit-info records the threshold portion only (the excess was
    // refunded). Fees on the threshold portion were already transferred
    // out by the dispatcher's `build_fee_messages`.
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
    // THEN remit any creation-fee reserve leftover to the bluechip wallet.
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
        .add_attribute("bluechip_excess_refunded", excess_refund.to_string())
        .add_attribute(
            "bluechip_excess_retained_for_creation_fee",
            excess_retained.to_string(),
        ))
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
    fee_swap_budget: Option<Uint128>,
    fee_cfg: Option<&Coin>,
    pricing_pool_id: u64,
    fee_quote_denom: &str,
    mut messages: Vec<CosmosMsg>,
    analytics: &PoolAnalytics,
) -> Result<Response, ContractError> {
    if IS_THRESHOLD_HIT.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::StuckThresholdProcessing);
    }

    // Insert the crosser into the ledger + bump COMMITTER_COUNT if new
    // before `trigger_threshold_payout` reads it below.
    super::record_committer(deps.storage, &sender, commit_value)?;
    let final_raised = new_total.min(commit_config.commit_amount_for_threshold_native);
    GROSS_NATIVE_COMMITTED.save(deps.storage, &final_raised)?;
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
        fee_swap_budget,
        fee_cfg,
        pricing_pool_id,
        fee_quote_denom,
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
    // remit any creation-fee reserve leftover.
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
