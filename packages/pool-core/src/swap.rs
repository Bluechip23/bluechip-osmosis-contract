//! Pair-shape-agnostic swap — now routed through the NATIVE Osmosis pool.
//!
//! Phase-2 replaced the internal constant-product AMM with a native GAMM
//! balancer pool created at threshold-crossing. A swap therefore no longer
//! does any reserve math locally. Instead it:
//!
//! 1. Confirms/derives the offer coin and the ask (`token_out`) denom from
//!    `POOL_INFO.asset_infos`.
//! 2. Derives a `token_out_min_amount` slippage floor from the caller's
//!    `belief_price` / `max_spread` (a floor of zero when no belief price
//!    is supplied — see [`derive_token_out_min`]).
//! 3. Dispatches `MsgSwapExactAmountIn` (built by
//!    `pool_core::osmosis_msgs::swap_exact_amount_in_msg`, `sender` = the
//!    pool contract) as a `SubMsg::reply_on_success` carrying the receiver
//!    in its `payload`.
//! 4. In the reply ([`handle_swap_forward_reply`]) it reads
//!    `MsgSwapExactAmountInResponse.token_out_amount` and `BankMsg::Send`s
//!    it to the receiver.
//!
//! The pool already holds the offer funds — they were attached to the
//! `SimpleSwap` message and confirmed via `confirm_sent_native_balance` at
//! the contract entry point — so `MsgSwapExactAmountIn` with the pool as
//! `sender` spends the pool's own balance.

use crate::asset::TokenInfo;
use crate::error::ContractError;
use crate::generic::{check_rate_limit, enforce_transaction_deadline, with_reentrancy_guard};
use crate::osmosis_msgs::swap_exact_amount_in_msg;
use crate::state::{
    PoolCtx, SwapForwardPayload, IS_THRESHOLD_HIT, POOL_ANALYTICS, POOL_ID, POOL_PAUSED,
    REPLY_ID_SWAP_FORWARD,
};
use cosmwasm_std::{
    to_json_binary, Addr, BankMsg, Coin, Decimal, DepsMut, Env, Fraction, MessageInfo, Reply,
    Response, StdError, StdResult, SubMsg, SubMsgResult, Uint128,
};
use osmosis_std::types::osmosis::poolmanager::v1beta1::MsgSwapExactAmountInResponse;
use prost::Message;
use std::str::FromStr;

pub const DEFAULT_SLIPPAGE: &str = "0.005";

/// Protobuf type URL for the poolmanager swap response.
const SWAP_EXACT_AMOUNT_IN_RESPONSE_TYPE: &str =
    "/osmosis.poolmanager.v1beta1.MsgSwapExactAmountInResponse";

/// Extract the ask (`token_out`) denom string from a `TokenType`.
fn denom_of(t: &crate::asset::TokenType) -> String {
    use crate::asset::TokenType;
    match t {
        TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom.clone(),
    }
}

/// Derive the `token_out_min_amount` slippage floor for a native-pool
/// swap from the caller's `belief_price` / `max_spread`.
///
/// `belief_price` is expressed as offer-per-ask (the same convention the
/// retired internal `assert_max_spread` used: `expected_ask =
/// offer_amount / belief_price`). We floor the acceptable output at
/// `expected_ask * (1 - max_spread)`.
///
/// When `belief_price` is `None` we return a floor of ZERO: without a
/// caller-supplied reference price there is nothing on-chain to derive a
/// price-impact bound from short of a spot-price query against the native
/// pool, and the native pool's own `token_out_min_amount` of zero matches
/// the prior "no belief price ⇒ only the max_spread-vs-spread heuristic"
/// behavior closely enough for a floor. Callers wanting a hard floor must
/// pass a `belief_price`. This is documented behavior, not an oversight.
pub fn derive_token_out_min(
    offer_amount: Uint128,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    allow_high_max_spread: Option<bool>,
) -> Result<Uint128, ContractError> {
    let default_spread = Decimal::from_str(DEFAULT_SLIPPAGE)?;
    let max_spread = max_spread.unwrap_or(default_spread);
    let hard_cap = if allow_high_max_spread.unwrap_or(false) {
        Decimal::percent(10)
    } else {
        Decimal::percent(5)
    };
    if max_spread > hard_cap {
        return Err(ContractError::MaxSpreadAssertion {});
    }
    if belief_price == Some(Decimal::zero()) {
        return Err(ContractError::InvalidBeliefPrice {});
    }

    let belief_price = match belief_price {
        Some(bp) => bp,
        // No reference price — floor of zero. See doc-comment.
        None => return Ok(Uint128::zero()),
    };

    // expected_ask = offer_amount / belief_price = offer_amount * inv(bp)
    let inverse = belief_price
        .inv()
        .ok_or_else(|| ContractError::Std(StdError::generic_err("Invalid belief price: zero")))?;
    let expected_ask = offer_amount
        .checked_mul(inverse.numerator())?
        .checked_div(inverse.denominator())
        .map_err(|_| ContractError::DivideByZero)?;

    // floor = expected_ask * (1 - max_spread)
    let keep = Decimal::one().checked_sub(max_spread).unwrap_or_default();
    let floor = expected_ask.multiply_ratio(keep.numerator(), keep.denominator());
    Ok(floor)
}

// ---------------------------------------------------------------------------
// Swap orchestration (reentrancy/rate-limit wrapper + native-pool dispatch)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn simple_swap(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    sender: Addr,
    offer_asset: TokenInfo,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    allow_high_max_spread: Option<bool>,
    to: Option<Addr>,
    transaction_deadline: Option<cosmwasm_std::Timestamp>,
    preloaded_ctx: Option<PoolCtx>,
) -> Result<Response, ContractError> {
    enforce_transaction_deadline(env.block.time, transaction_deadline)?;

    with_reentrancy_guard(deps, move |mut deps| {
        if !IS_THRESHOLD_HIT.load(deps.storage)? {
            return Err(ContractError::ShortOfThreshold {});
        }
        let ctx = match preloaded_ctx {
            Some(ctx) => ctx,
            None => PoolCtx::load(deps.storage)?,
        };
        execute_simple_swap_with_ctx(
            &mut deps,
            env,
            info,
            sender,
            offer_asset,
            belief_price,
            max_spread,
            allow_high_max_spread,
            to,
            ctx,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub fn execute_simple_swap(
    deps: &mut DepsMut,
    env: Env,
    info: MessageInfo,
    sender: Addr,
    offer_asset: TokenInfo,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    allow_high_max_spread: Option<bool>,
    to: Option<Addr>,
) -> Result<Response, ContractError> {
    if !IS_THRESHOLD_HIT.load(deps.storage)? {
        return Err(ContractError::ShortOfThreshold {});
    }
    let ctx = PoolCtx::load(deps.storage)?;
    execute_simple_swap_with_ctx(
        deps,
        env,
        info,
        sender,
        offer_asset,
        belief_price,
        max_spread,
        allow_high_max_spread,
        to,
        ctx,
    )
}

/// Builds the native-pool swap SubMsg. Callers enforce the
/// IS_THRESHOLD_HIT gate before delegating here.
#[allow(clippy::too_many_arguments)]
fn execute_simple_swap_with_ctx(
    deps: &mut DepsMut,
    env: Env,
    _info: MessageInfo,
    sender: Addr,
    offer_asset: TokenInfo,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    allow_high_max_spread: Option<bool>,
    to: Option<Addr>,
    ctx: PoolCtx,
) -> Result<Response, ContractError> {
    let PoolCtx {
        info: pool_info,
        state: _pool_state,
        specs: pool_specs,
    } = ctx;

    check_rate_limit(deps, &env, &pool_specs, &sender)?;

    // Resolve which side is being offered and the ask denom.
    let (offer_denom, ask_denom) =
        if offer_asset.info.equal(&pool_info.pool_info.asset_infos[0]) {
            (
                denom_of(&pool_info.pool_info.asset_infos[0]),
                denom_of(&pool_info.pool_info.asset_infos[1]),
            )
        } else if offer_asset.info.equal(&pool_info.pool_info.asset_infos[1]) {
            (
                denom_of(&pool_info.pool_info.asset_infos[1]),
                denom_of(&pool_info.pool_info.asset_infos[0]),
            )
        } else {
            return Err(ContractError::AssetMismatch {});
        };

    if POOL_PAUSED.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::PoolPausedLowLiquidity {});
    }

    // The native pool id must be set (it is, once IS_THRESHOLD_HIT is true
    // and the create-pool reply landed).
    let pool_id = POOL_ID
        .may_load(deps.storage)?
        .ok_or(ContractError::ShortOfThreshold {})?;

    let token_out_min_amount =
        derive_token_out_min(offer_asset.amount, belief_price, max_spread, allow_high_max_spread)?;

    let token_in = Coin {
        denom: offer_denom.clone(),
        amount: offer_asset.amount,
    };
    let pool_addr = pool_info.pool_info.contract_addr.clone();
    let receiver = to.unwrap_or_else(|| sender.clone());

    let payload = SwapForwardPayload {
        receiver: receiver.clone(),
        token_out_denom: ask_denom.clone(),
        sender: sender.clone(),
        offer_amount: offer_asset.amount,
        offer_denom: offer_denom.clone(),
    };

    let swap_msg = swap_exact_amount_in_msg(
        &pool_addr,
        pool_id,
        &token_in,
        &ask_denom,
        token_out_min_amount,
    );
    let submsg = SubMsg::reply_on_success(swap_msg, REPLY_ID_SWAP_FORWARD)
        .with_payload(to_json_binary(&payload)?);

    // Bump swap analytics optimistically (offer-side volume known now;
    // ask-side volume is finalized in the reply). If the swap SubMsg
    // fails, the whole tx reverts and this write is rolled back.
    let mut analytics = POOL_ANALYTICS.may_load(deps.storage)?.unwrap_or_default();
    analytics.total_swap_count += 1;
    if offer_denom == denom_of(&pool_info.pool_info.asset_infos[0]) {
        analytics.total_volume_0 = analytics.total_volume_0.saturating_add(offer_asset.amount);
    } else {
        analytics.total_volume_1 = analytics.total_volume_1.saturating_add(offer_asset.amount);
    }
    analytics.last_trade_block = env.block.height;
    analytics.last_trade_timestamp = env.block.time.seconds();
    POOL_ANALYTICS.save(deps.storage, &analytics)?;

    Ok(Response::new().add_submessage(submsg).add_attributes(vec![
        ("action", "swap".to_string()),
        ("sender", sender.to_string()),
        ("receiver", receiver.to_string()),
        ("offer_asset", offer_denom),
        ("ask_asset", ask_denom),
        ("offer_amount", offer_asset.amount.to_string()),
        ("token_out_min_amount", token_out_min_amount.to_string()),
        ("pool_id", pool_id.to_string()),
        ("pool_contract", pool_addr.to_string()),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
    ]))
}

/// Decode a `MsgSwapExactAmountInResponse` from a reply's SubMsg result,
/// preferring the typed `msg_responses` entry and falling back to the
/// deprecated `data` field for pre-CosmWasm-2.0 chains. Returns the
/// `token_out_amount`.
pub fn parse_swap_out_amount(result: &SubMsgResult) -> StdResult<Uint128> {
    let response = result.clone().into_result().map_err(StdError::generic_err)?;

    let bytes: Vec<u8> = response
        .msg_responses
        .iter()
        .find(|r| r.type_url == SWAP_EXACT_AMOUNT_IN_RESPONSE_TYPE)
        .map(|r| r.value.to_vec())
        .or_else(|| {
            #[allow(deprecated)]
            response.data.as_ref().map(|d| d.to_vec())
        })
        .ok_or_else(|| {
            StdError::generic_err("swap reply: no MsgSwapExactAmountInResponse in reply")
        })?;

    let decoded = MsgSwapExactAmountInResponse::decode(bytes.as_slice()).map_err(|e| {
        StdError::generic_err(format!("swap reply: failed to decode response: {}", e))
    })?;

    Uint128::from_str(&decoded.token_out_amount).map_err(|e| {
        StdError::generic_err(format!("swap reply: invalid token_out_amount: {}", e))
    })
}

/// Reply handler for `REPLY_ID_SWAP_FORWARD`: forward the swapped-out
/// tokens to the receiver recorded in the SubMsg payload, and finalize
/// the ask-side analytics volume.
pub fn handle_swap_forward_reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    let payload: SwapForwardPayload = cosmwasm_std::from_json(&msg.payload)?;
    let token_out_amount = parse_swap_out_amount(&msg.result)?;

    // Finalize ask-side volume (the offer side was recorded at dispatch).
    let pool_info = POOL_INFO_LOAD(deps.storage)?;
    let mut analytics = POOL_ANALYTICS.may_load(deps.storage)?.unwrap_or_default();
    let asset0_denom = denom_of(&pool_info.pool_info.asset_infos[0]);
    if payload.token_out_denom == asset0_denom {
        analytics.total_volume_0 = analytics.total_volume_0.saturating_add(token_out_amount);
    } else {
        analytics.total_volume_1 = analytics.total_volume_1.saturating_add(token_out_amount);
    }
    POOL_ANALYTICS.save(deps.storage, &analytics)?;

    let mut msgs: Vec<cosmwasm_std::CosmosMsg> = vec![];
    if !token_out_amount.is_zero() {
        msgs.push(cosmwasm_std::CosmosMsg::Bank(BankMsg::Send {
            to_address: payload.receiver.to_string(),
            amount: vec![Coin {
                denom: payload.token_out_denom.clone(),
                amount: token_out_amount,
            }],
        }));
    }

    let effective_price = if !payload.offer_amount.is_zero() {
        Decimal::from_ratio(token_out_amount, payload.offer_amount).to_string()
    } else {
        "0".to_string()
    };

    Ok(Response::new().add_messages(msgs).add_attributes(vec![
        ("action", "swap_forward".to_string()),
        ("sender", payload.sender.to_string()),
        ("receiver", payload.receiver.to_string()),
        ("offer_amount", payload.offer_amount.to_string()),
        ("offer_denom", payload.offer_denom),
        ("return_amount", token_out_amount.to_string()),
        ("token_out_denom", payload.token_out_denom),
        ("effective_price", effective_price),
        ("block_time", env.block.time.seconds().to_string()),
    ]))
}

// Small load helper so the reply doesn't need the full POOL_INFO import
// surface duplicated; keeps the module's import list minimal.
#[allow(non_snake_case)]
fn POOL_INFO_LOAD(storage: &dyn cosmwasm_std::Storage) -> StdResult<crate::state::PoolInfo> {
    crate::state::POOL_INFO.load(storage)
}
