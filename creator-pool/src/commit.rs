//! Commit entry point + dispatcher, plus shared per-commit helpers
//! (fee split, fee-message builder, response-attribute base).
//!
//! The four handler bodies — pre-threshold funding, post-threshold AMM
//! swap, threshold-crossing split, and distribution batch processing —
//! live in submodules:
//! - [`pre_threshold`]       — commits while the pool is still funding
//! - [`post_threshold`]      — commits after the pool is fully funded
//! - [`threshold_crossing`]  — the commit that carries the pool across
//! - [`distribution`]        — post-threshold keeper-driven payout batches
//!
//! This file keeps:
//! - `commit` / `execute_commit_logic` — the entry point + dispatcher
//! - `commit_base_attributes`          — shared by all four response paths
//! - `calculate_commit_fees` / `build_fee_messages`
//! - `MIN_COMMIT_USD_*` constants
//!
//! and re-exports `execute_continue_distribution` so the pool's entry
//! points don't need to know about the submodule structure.

pub mod distribution;
pub mod distribution_batch;
pub mod post_threshold;
pub mod pre_threshold;
pub mod threshold_crossing;
pub mod threshold_payout;

pub use distribution::execute_continue_distribution;

use cosmwasm_std::{
    Addr, CosmosMsg, Decimal, DepsMut, Env, Fraction, MessageInfo, Response, Timestamp, Uint128,
};

use crate::admin::ensure_not_drained;
use crate::asset::{get_native_denom, TokenInfo, TokenType};
use crate::error::ContractError;
use crate::generic_helpers::{
    check_rate_limit, enforce_transaction_deadline, get_bank_transfer_to_msg, with_reentrancy_guard,
};
use crate::msg::CommitFeeInfo;
use crate::state::{
    PoolSpecs, COMMITFEEINFO, COMMIT_LIMIT_INFO, IS_THRESHOLD_HIT, LAST_THRESHOLD_ATTEMPT,
    POOL_ANALYTICS, POOL_FEE_STATE, POOL_INFO, POOL_PAUSED, POOL_SPECS, POOL_STATE,
    THRESHOLD_PAYOUT_AMOUNTS, THRESHOLD_PROCESSING, USD_RAISED_FROM_COMMIT,
};

use crate::swap_helper::get_usd_conversion;

use post_threshold::process_post_threshold_commit;
use pre_threshold::process_pre_threshold_commit;
use threshold_crossing::{process_threshold_crossing_with_excess, process_threshold_hit_exact};

// Minimum commit-value floors are per-pool state. Defaults are
// `crate::state::DEFAULT_MIN_COMMIT_USD_{PRE,POST}_THRESHOLD` and the
// active values are stored on `CommitLimitInfo.min_commit_usd_pre_threshold`
// / `min_commit_usd_post_threshold`. The floor still limits pre-threshold
// ledger bloat (an attacker can cross the threshold with their own
// money, but not via thousands of micro-entries that balloon the
// distribution queue); post-threshold commits stay looser since they're
// AMM swaps that don't touch COMMIT_LEDGER.

/// Base attribute set shared by every commit response (pre-threshold,
/// post-threshold, threshold_hit_exact, threshold_crossing). Each caller
/// adds its path-specific attributes on top via `Response::add_attributes`.
///
/// Returned as `Vec<(&str, String)>` for consistency with the
/// tuple-vec form used elsewhere in this crate (admin response
/// builders, liquidity_helpers claim handlers). `Response::add_attributes`
/// accepts any `IntoIterator<Item = impl Into<Attribute>>` so the
/// consuming sites are unchanged.
pub(crate) fn commit_base_attributes(
    phase: &'static str,
    sender: &Addr,
    pool_contract: &Addr,
    total_commit_count: u64,
    env: &Env,
) -> Vec<(&'static str, String)> {
    vec![
        ("action", "commit".to_string()),
        ("phase", phase.to_string()),
        ("committer", sender.to_string()),
        ("total_commit_count", total_commit_count.to_string()),
        ("pool_contract", pool_contract.to_string()),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
    ]
}

pub fn commit(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    asset: TokenInfo,
    transaction_deadline: Option<Timestamp>,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
) -> Result<Response, ContractError> {
    ensure_not_drained(deps.storage)?;
    // admin (or auto-low-liquidity) pause halts ALL
    // commit branches, not just the post-threshold AMM-swap path.
    // POOL_PAUSED is true whenever the pool is paused for any reason
    // (admin Pause, emergency-withdraw Phase 1, or auto-pause from
    // reserves dipping below MINIMUM_LIQUIDITY); POOL_PAUSED_AUTO is
    // a discriminator that doesn't matter at the commit gate. Without
    // this check, a paused pool would continue to bank pre-threshold
    // funds and to cross the threshold while admin investigates —
    // a fire-alarm-with-foot-still-on-the-gas failure mode. The
    // redundant check in `process_post_threshold_commit` is
    // defense-in-depth. Reuses the `PoolPausedLowLiquidity` error
    // variant for consistency with the swap and post-threshold
    // callers; the variant name calls out only the auto-low-liquidity
    // pause path but is shared by all of them.
    if POOL_PAUSED.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::PoolPausedLowLiquidity {});
    }
    enforce_transaction_deadline(env.block.time, transaction_deadline)?;

    with_reentrancy_guard(deps, |mut deps| {
        let pool_specs = POOL_SPECS.load(deps.storage)?;
        let sender = info.sender.clone();
        check_rate_limit(&mut deps, &env, &pool_specs, &sender)?;
        // Hand the already-loaded POOL_SPECS to the dispatcher so it
        // doesn't re-read the same item.
        execute_commit_logic(
            &mut deps,
            env,
            info,
            asset,
            belief_price,
            max_spread,
            pool_specs,
        )
    })
}

fn execute_commit_logic(
    deps: &mut DepsMut,
    env: Env,
    info: MessageInfo,
    asset: TokenInfo,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    pool_specs: PoolSpecs,
) -> Result<Response, ContractError> {
    let amount = asset.amount;
    let pool_info = POOL_INFO.load(deps.storage)?;
    let mut pool_state = POOL_STATE.load(deps.storage)?;
    let commit_config = COMMIT_LIMIT_INFO.load(deps.storage)?;
    let fee_info = COMMITFEEINFO.load(deps.storage)?;
    let sender = info.sender.clone();

    // Resolve the LIVE bluechip protocol-wallet for both the per-commit
    // fee transfer and the threshold-cross bluechip-reward mint. The
    // pool's `COMMITFEEINFO.bluechip_wallet_address` is snapshotted at
    // create time; the factory's address is admin-tunable via the
    // standard 48h `ProposeConfigUpdate` flow. Querying live here keeps
    // both fund flows in lockstep with a key-compromise-driven wallet
    // rotation — without it, every existing pool would keep paying the
    // protocol fee and the 25k-token threshold-cross reward to the old
    // (potentially compromised) wallet indefinitely. Mirrors the live-
    // query pattern already in use on the emergency-drain recipient
    // (pool-core::admin). Fail-soft fallback to the snapshot keeps
    // commits live if the factory is unreachable.
    let live_bluechip_wallet = crate::generic_helpers::resolve_live_bluechip_wallet(
        deps.as_ref(),
        &pool_info.factory_addr,
        &fee_info.bluechip_wallet_address,
    );

    // commits flow only in the bluechip direction.
    // `validate_pool_token_info` pins `asset_infos[0]` to the canonical
    // bluechip Native denom and `asset_infos[1]` to the creator-token
    // CW20, and the inner `match` below only handles bluechip Native,
    // returning `AssetMismatch` for everything else. The outer check is
    // bluechip-only so a caller passing the creator-token side surfaces
    // the clearer error earlier and skips the USD-conversion +
    // min-commit + analytics work that would otherwise run before the
    // inner reject. The inner `_ => AssetMismatch` arm remains as
    // defense-in-depth against config corruption.
    if !asset.info.equal(&pool_info.pool_info.asset_infos[0]) {
        return Err(ContractError::AssetMismatch {});
    }
    if asset.amount.is_zero() {
        return Err(ContractError::ZeroAmount {});
    }

    // Value the GROSS (pre-fee) commit in USD once at entry and thread
    // the same rate through every conversion in this handler. The rate
    // comes from the factory's ConvertNativeToUsd query, backed by the
    // chain-native x/twap of the configured native/USD-stable pool —
    // one query per commit, no keeper, and no mid-tx drift because the
    // threshold split below reuses `usd_rate` rather than re-querying.
    let conversion = get_usd_conversion(deps.as_ref(), asset.amount)?;
    let commit_value = conversion.amount;
    let usd_rate = conversion.rate_used;
    if usd_rate.is_zero() || commit_value.is_zero() {
        return Err(ContractError::InvalidOraclePrice {});
    }
    // Load IS_THRESHOLD_HIT once and thread it through both the minimum-
    // commit check here and the main branching below (used later as
    // `threshold_already_hit`).
    let threshold_already_hit = IS_THRESHOLD_HIT.load(deps.storage)?;
    let min_commit = if threshold_already_hit {
        commit_config.min_commit_usd_post_threshold
    } else {
        commit_config.min_commit_usd_pre_threshold
    };
    if commit_value < min_commit {
        let phase: &'static str = if threshold_already_hit {
            "post-threshold"
        } else {
            "pre-threshold"
        };
        return Err(ContractError::CommitTooSmall {
            got: commit_value,
            min: min_commit,
            phase,
        });
    }

    let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;

    match &asset.info {
        TokenType::Native { denom } if denom == &bluechip_denom => {
            // Strict exact-match on attached funds via `cw_utils::must_pay`.
            //
            // `must_pay` enforces:
            // 1. Funds list must be exactly one coin (rejects multi-denom).
            // An attacker (or careless frontend) attaching
            // `[ubluechip: amount, ibc/...: Y]` would otherwise have the
            // IBC denom silently absorbed into the pool's bank balance
            // with no recovery path.
            // 2. Coin amount must be non-zero.
            // 3. Coin denom must match the canonical bluechip denom.
            //
            // The post-condition `sent == amount` then catches under/
            // overpayment in the bluechip side, preserving the
            // exact-amount semantics that `simple_swap` already enforces
            // via `confirm_sent_native_balance` (which delegates to
            // must_pay too).
            let sent = cw_utils::must_pay(&info, denom.as_str()).map_err(|e| {
                ContractError::InvalidCommitFunds {
                    reason: e.to_string(),
                }
            })?;
            if sent != amount {
                return Err(ContractError::MismatchAmount {});
            }

            let (commit_fee_bluechip_amt, commit_fee_creator_amt) =
                calculate_commit_fees(amount, &fee_info)?;
            let total_fees = commit_fee_bluechip_amt.checked_add(commit_fee_creator_amt)?;
            if total_fees >= amount {
                return Err(ContractError::InvalidFee {});
            }
            let amount_after_fees = amount.checked_sub(total_fees)?;
            if amount_after_fees.is_zero() {
                return Err(ContractError::InvalidFee {});
            }

            let messages = build_fee_messages(
                &fee_info,
                &live_bluechip_wallet,
                denom,
                commit_fee_bluechip_amt,
                commit_fee_creator_amt,
            )?;

            // Load `POOL_ANALYTICS` once for this dispatch path; the
            // `total_commit_count` bump is universal to every commit
            // branch below, so we increment here and let each handler
            // mutate swap-specific fields on the shared `&mut analytics`.
            // A single save at the bottom of the Native arm persists the
            // result for all four phase handlers.
            let mut analytics = POOL_ANALYTICS.may_load(deps.storage)?.unwrap_or_default();
            analytics.total_commit_count += 1;

            // `threshold_already_hit` was loaded above alongside the
            // minimum-commit check — reuse it here instead of re-reading.
            let response = if !threshold_already_hit {
                let current_raised = USD_RAISED_FROM_COMMIT.load(deps.storage)?;
                let new_total = current_raised.checked_add(commit_value)?;

                if new_total >= commit_config.commit_amount_for_threshold_usd {
                    LAST_THRESHOLD_ATTEMPT.save(deps.storage, &env.block.time)?;

                    // THRESHOLD_PROCESSING is set to `true` immediately
                    // below, then cleared at the end of the threshold-
                    // crossing path (excess or exact-hit branch). If the
                    // crossing handler errors, the entire tx reverts —
                    // including this `save(true)` — so the storage
                    // reverts to whatever it was before this tx (which
                    // was `false`). REENTRANCY_LOCK separately blocks
                    // any in-tx reentry. Net: under normal operation,
                    // `THRESHOLD_PROCESSING == true` at this point is
                    // structurally unreachable.
                    //
                    // The only way to observe a stuck `true` is genuine
                    // storage corruption (unrecoverable bug) or an
                    // interrupted prior tx that somehow committed without
                    // clearing the flag (would also indicate a bug).
                    // Rather than silently downgrading the user's intended
                    // threshold-crossing commit into a pre/post-threshold
                    // commit (which would violate user intent and hide
                    // the underlying corruption), surface the stuck
                    // state with an explicit error pointing operators at
                    // the recovery path.
                    if THRESHOLD_PROCESSING
                        .may_load(deps.storage)?
                        .unwrap_or(false)
                    {
                        return Err(ContractError::StuckThresholdProcessing);
                    }
                    THRESHOLD_PROCESSING.save(deps.storage, &true)?;

                    // These two items are consumed only by the crossing
                    // handlers, which run exactly once per pool lifetime
                    // — load them here rather than on every commit so
                    // the hot pre-/post-threshold paths never pay for
                    // reads they don't use.
                    let threshold_payout = THRESHOLD_PAYOUT_AMOUNTS.load(deps.storage)?;
                    let mut pool_fee_state = POOL_FEE_STATE.load(deps.storage)?;

                    let value_to_threshold = commit_config
                        .commit_amount_for_threshold_usd
                        .checked_sub(current_raised)
                        .unwrap_or(Uint128::zero());

                    if commit_value > value_to_threshold && value_to_threshold > Uint128::zero() {
                        // Split commit: part goes to threshold, excess becomes swap
                        process_threshold_crossing_with_excess(
                            deps,
                            env,
                            sender,
                            &asset,
                            amount,
                            amount_after_fees,
                            commit_value,
                            value_to_threshold,
                            usd_rate,
                            &mut pool_state,
                            &mut pool_fee_state,
                            &pool_specs,
                            &pool_info,
                            &commit_config,
                            &threshold_payout,
                            &fee_info,
                            &live_bluechip_wallet,
                            messages,
                            belief_price,
                            max_spread,
                            &mut analytics,
                        )?
                    } else {
                        // Threshold hit exactly — handled by
                        // `commit::threshold_crossing::process_threshold_hit_exact`
                        // so all the phase handlers sit at the same module
                        // depth (pre / post / threshold-with-excess /
                        // threshold-hit-exact / distribution batch).
                        process_threshold_hit_exact(
                            deps,
                            env,
                            sender,
                            &asset,
                            amount_after_fees,
                            commit_value,
                            new_total,
                            &mut pool_state,
                            &mut pool_fee_state,
                            &pool_info,
                            &commit_config,
                            &threshold_payout,
                            &fee_info,
                            &live_bluechip_wallet,
                            messages,
                            &analytics,
                        )?
                    }
                } else {
                    process_pre_threshold_commit(
                        deps,
                        env,
                        sender,
                        &asset,
                        commit_value,
                        // Net-of-fees bluechip that actually enters the
                        // contract bank balance from this commit
                        // (see pre_threshold.rs).
                        amount_after_fees,
                        // Already-computed USD_RAISED_FROM_COMMIT +
                        // commit_value, so the handler saves the new
                        // total without re-reading the item.
                        new_total,
                        messages,
                        &pool_state,
                        &mut analytics,
                    )?
                }
            } else {
                // Loaded here rather than at the top of the dispatcher:
                // the pre-threshold path never touches fee state, so
                // only the post-threshold (and crossing) branches pay
                // for this read.
                let mut pool_fee_state = POOL_FEE_STATE.load(deps.storage)?;
                process_post_threshold_commit(
                    deps,
                    env,
                    sender,
                    asset,
                    amount_after_fees,
                    commit_value,
                    messages,
                    belief_price,
                    max_spread,
                    &pool_info,
                    &pool_specs,
                    &mut pool_state,
                    &mut pool_fee_state,
                    &mut analytics,
                )?
            };

            // Single analytics save covers every commit branch. If
            // anything above returned `Err`, the whole tx aborts
            // (CosmWasm storage is transactional), so this save
            // never persists in error paths.
            POOL_ANALYTICS.save(deps.storage, &analytics)?;
            Ok(response)
        }
        _ => Err(ContractError::AssetMismatch {}),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Calculate both fee portions for a commit. Returns (bluechip_fee, creator_fee).
fn calculate_commit_fees(
    amount: Uint128,
    fee_info: &CommitFeeInfo,
) -> Result<(Uint128, Uint128), ContractError> {
    let bluechip_fee = amount
        .checked_mul(fee_info.commit_fee_bluechip.numerator())?
        .checked_div(fee_info.commit_fee_bluechip.denominator())
        .map_err(|_| ContractError::DivideByZero)?;
    let creator_fee = amount
        .checked_mul(fee_info.commit_fee_creator.numerator())?
        .checked_div(fee_info.commit_fee_creator.denominator())
        .map_err(|_| ContractError::DivideByZero)?;
    Ok((bluechip_fee, creator_fee))
}

/// Build bank-send messages for the two fee recipients.
///
/// `bluechip_wallet` is resolved at the caller via
/// `generic_helpers::resolve_live_bluechip_wallet` so the protocol-fee
/// destination tracks the LIVE factory config rather than the snapshot
/// pinned in `fee_info.bluechip_wallet_address` at pool create. This
/// keeps an admin wallet rotation (e.g., after a key compromise) actually
/// effective for every pre-existing pool's commit-fee stream.
///
/// `fee_info.creator_wallet_address` stays as-is — the creator wallet is
/// per-pool, set from the pool's creator at instantiate, and is not a
/// protocol-level rotation target.
fn build_fee_messages(
    fee_info: &CommitFeeInfo,
    bluechip_wallet: &Addr,
    denom: &str,
    bluechip_fee: Uint128,
    creator_fee: Uint128,
) -> Result<Vec<CosmosMsg>, ContractError> {
    let mut messages = Vec::new();
    if !bluechip_fee.is_zero() {
        messages.push(get_bank_transfer_to_msg(
            bluechip_wallet,
            denom,
            bluechip_fee,
        )?);
    }
    if !creator_fee.is_zero() {
        messages.push(get_bank_transfer_to_msg(
            &fee_info.creator_wallet_address,
            denom,
            creator_fee,
        )?);
    }
    Ok(messages)
}
