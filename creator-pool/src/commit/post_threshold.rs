//! Post-threshold commit handler. Once the pool has crossed its commit
//! threshold, the commit flow's swap leg routes through the NATIVE Osmosis
//! pool: the caller's post-fee bluechip is swapped for the creator token
//! via `MsgSwapExactAmountIn`, and the reply forwards the creator tokens
//! back to the committer.
//!
//! The per-commit 1%/5% fee kickout and the `update_commit_info`
//! subscription record are preserved (the fee split happens in the
//! dispatcher; `swap_amount` here is already net-of-fees). The retired
//! internal-AMM machinery (compute_swap, reserve drain guards, the
//! post-threshold cooldown + swap-cap ramp) is gone.

use cosmwasm_std::{to_json_binary, Addr, Coin, CosmosMsg, Decimal, DepsMut, Env, Response, SubMsg, Uint128};

use crate::asset::{get_native_denom, TokenInfo};
use crate::error::ContractError;
use crate::generic_helpers::update_commit_info;
use crate::state::{
    PoolAnalytics, PoolInfo, PoolSpecs, POOL_ID, POOL_PAUSED, REPLY_ID_SWAP_FORWARD,
};
use crate::swap_helper::compute_token_out_min;
use pool_core::osmosis_msgs::swap_exact_amount_in_msg;
use pool_core::state::SwapForwardPayload;

use super::commit_base_attributes;

#[allow(clippy::too_many_arguments)]
pub(super) fn process_post_threshold_commit(
    deps: &mut DepsMut,
    env: Env,
    sender: Addr,
    asset: TokenInfo,
    swap_amount: Uint128,
    commit_value: Uint128,
    mut messages: Vec<CosmosMsg>,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    pool_info: &PoolInfo,
    _pool_specs: &PoolSpecs,
    analytics: &mut PoolAnalytics,
) -> Result<Response, ContractError> {
    if POOL_PAUSED.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::PoolPausedLowLiquidity {});
    }

    if swap_amount.is_zero() {
        return Err(ContractError::ZeroAmount {});
    }

    // The native pool id must be set post-threshold.
    let pool_id = POOL_ID
        .may_load(deps.storage)?
        .ok_or(ContractError::ShortOfThreshold {})?;

    let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;
    let creator_denom = pool_info.token_denom.clone();

    let token_in = Coin {
        denom: bluechip_denom.clone(),
        amount: swap_amount,
    };

    // Slippage floor = max(on-chain-estimate floor, belief-price floor).
    // Shares the exact orchestration used by `SimpleSwap` (pool_core::swap)
    // so the sandwich/slippage protection is identical at both sites.
    let token_out_min_amount = compute_token_out_min(
        &deps.querier,
        pool_id,
        &token_in,
        &creator_denom,
        belief_price,
        max_spread,
        None,
    )?;
    let payload = SwapForwardPayload {
        receiver: sender.clone(),
        token_out_denom: creator_denom.clone(),
        sender: sender.clone(),
        offer_amount: swap_amount,
        offer_denom: bluechip_denom.clone(),
    };
    let swap_msg = swap_exact_amount_in_msg(
        &pool_info.pool_info.contract_addr,
        pool_id,
        &token_in,
        &creator_denom,
        token_out_min_amount,
    );
    let swap_submsg = SubMsg::reply_on_success(swap_msg, REPLY_ID_SWAP_FORWARD)
        .with_payload(to_json_binary(&payload)?);

    // Subscription record (unchanged). `asset.amount` is the GROSS commit.
    update_commit_info(
        deps.storage,
        &sender,
        &pool_info.pool_info.contract_addr,
        asset.amount,
        commit_value,
        env.block.time,
    )?;

    // Analytics — offer-side volume known now; ask side finalized in the
    // swap-forward reply.
    analytics.total_swap_count += 1;
    analytics.total_volume_0 = analytics.total_volume_0.saturating_add(swap_amount);
    analytics.last_trade_block = env.block.height;
    analytics.last_trade_timestamp = env.block.time.seconds();

    let base = commit_base_attributes(
        "active",
        &sender,
        &pool_info.pool_info.contract_addr,
        analytics.total_commit_count,
        &env,
    );
    Ok(Response::new()
        .add_messages(messages.drain(..))
        .add_submessage(swap_submsg)
        .add_attributes(base)
        .add_attribute("commit_amount_bluechip", asset.amount.to_string())
        .add_attribute("swap_amount_bluechip", swap_amount.to_string())
        .add_attribute("token_out_min_amount", token_out_min_amount.to_string())
        .add_attribute("pool_id", pool_id.to_string()))
}
