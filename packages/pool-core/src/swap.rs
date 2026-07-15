//! Pair-shape-agnostic swap — now routed through the NATIVE Osmosis pool.
//!
//! Phase-2 replaced the internal constant-product AMM with a native GAMM
//! balancer pool created at threshold-crossing. A swap therefore no longer
//! does any reserve math locally. Instead it:
//!
//! 1. Confirms/derives the offer coin and the ask (`token_out`) denom from
//!    `POOL_INFO.asset_infos`.
//! 2. Derives a `token_out_min_amount` slippage floor as the MORE
//!    PROTECTIVE of an on-chain poolmanager estimate floor and the
//!    caller's `belief_price` floor (see [`compute_token_out_min`] /
//!    [`derive_token_out_min`]). The estimate floor binds even when no
//!    `belief_price` is supplied, so the swap never dispatches with
//!    `token_out_min_amount = 0`.
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
    PoolCtx, SwapForwardPayload, BREAKER_FLOOR_PERCENT, IS_THRESHOLD_HIT, POOL_ANALYTICS, POOL_ID,
    POOL_PAUSED, POOL_PAUSED_AUTO, REPLY_ID_SWAP_FORWARD, SEED_LIQUIDITY,
};
use cosmwasm_std::{
    to_json_binary, Addr, BankMsg, Coin, CustomQuery, Decimal, DepsMut, Env, Fraction, MessageInfo,
    QuerierWrapper, Reply, Response, StdError, StdResult, Storage, SubMsg, SubMsgResult, Uint128,
};
use osmosis_std::types::osmosis::poolmanager::v1beta1::{
    MsgSwapExactAmountInResponse, PoolmanagerQuerier, SwapAmountInRoute,
};
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
/// swap, taking the MORE PROTECTIVE (max) of two independently-derived
/// floors:
///
/// 1. an **on-chain estimate floor** — `estimated_out * (1 -
///    effective_max_spread)`, where `estimated_out` is the expected output
///    at CURRENT pool state (queried at the call site via
///    [`estimate_swap_out`] — see [`compute_token_out_min`]); and
/// 2. a **belief-price floor** — `expected_ask * (1 - effective_max_spread)`
///    where `expected_ask = offer_amount / belief_price` (the same
///    convention the retired internal `assert_max_spread` used), only when
///    the caller supplies a `belief_price`.
///
/// The result is `max(estimate_floor, belief_floor)`. This closes the
/// prior "no belief price ⇒ floor of zero (no sandwich/slippage
/// protection)" hole: even a caller that passes no `belief_price` now
/// gets the estimate-derived floor, so `MsgSwapExactAmountIn` never
/// dispatches with `token_out_min_amount = 0` against a functioning pool.
///
/// The function stays PURE and testable: the on-chain estimate is passed
/// in as `estimated_out` (the query is done by the caller, which holds the
/// querier + pool_id + denoms). A zero `estimated_out` (e.g. the estimate
/// query was unavailable) simply contributes a zero estimate floor, so
/// `max(0, belief_floor) == belief_floor` — the belief floor still binds.
///
/// The existing hard-cap validation (5% / 10%-with-`allow_high`) and the
/// zero-belief-price rejection are preserved: a `max_spread` above the cap
/// still errors, and a `belief_price` of exactly zero is rejected.
pub fn derive_token_out_min(
    offer_amount: Uint128,
    estimated_out: Uint128,
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

    // `effective_max_spread` is clamped to the hard cap. After the check
    // above this equals `max_spread`, but computing the min explicitly
    // keeps the floor safe even if the cap logic is ever relaxed to warn
    // instead of reject.
    let effective_max_spread = max_spread.min(hard_cap);
    let keep = Decimal::one()
        .checked_sub(effective_max_spread)
        .unwrap_or_default();

    // (1) On-chain estimate floor: `estimated_out * (1 - spread)`.
    let estimate_floor = estimated_out.multiply_ratio(keep.numerator(), keep.denominator());

    // (2) Belief-price floor: `(offer / belief_price) * (1 - spread)`.
    let belief_floor = match belief_price {
        Some(bp) => {
            // expected_ask = offer_amount / belief_price = offer_amount * inv(bp)
            let inverse = bp.inv().ok_or_else(|| {
                ContractError::Std(StdError::generic_err("Invalid belief price: zero"))
            })?;
            let expected_ask = offer_amount
                .checked_mul(inverse.numerator())?
                .checked_div(inverse.denominator())
                .map_err(|_| ContractError::DivideByZero)?;
            expected_ask.multiply_ratio(keep.numerator(), keep.denominator())
        }
        None => Uint128::zero(),
    };

    // Take the MORE PROTECTIVE of the two floors.
    Ok(estimate_floor.max(belief_floor))
}

/// Query the poolmanager for the expected output of swapping `token_in`
/// for `ask_denom` through `pool_id` at CURRENT pool state.
///
/// FAIL-SOFT: any query error (the estimate endpoint being unavailable on
/// a given chain build, a transient failure, etc.) resolves to `zero`,
/// which [`derive_token_out_min`] treats as "no estimate floor" so the
/// belief-price floor and the hard-cap validation still bind. On a
/// functioning Osmosis pool the estimate always resolves, so the estimate
/// floor is the load-bearing protection whenever the caller passes no
/// `belief_price`.
pub fn estimate_swap_out<C: CustomQuery>(
    querier: &QuerierWrapper<C>,
    pool_id: u64,
    token_in: &Coin,
    ask_denom: &str,
) -> Uint128 {
    // Osmosis coin string is `{amount}{denom}` (e.g. "100000000ubluechip").
    let token_in_str = format!("{}{}", token_in.amount, token_in.denom);
    let routes = vec![SwapAmountInRoute {
        pool_id,
        token_out_denom: ask_denom.to_string(),
    }];
    // `sender` is DEPRECATED on the request and unused by the estimate;
    // pass empty.
    match PoolmanagerQuerier::new(querier)
        .estimate_swap_exact_amount_in(String::new(), pool_id, token_in_str, routes)
    {
        Ok(resp) => Uint128::from_str(&resp.token_out_amount).unwrap_or_default(),
        Err(_) => Uint128::zero(),
    }
}

/// Shared orchestration for BOTH swap sites (SimpleSwap and the
/// post-threshold commit swap): query the on-chain estimate, then derive
/// the `max(estimate_floor, belief_floor)` slippage floor. Keeps the two
/// call sites byte-identical so the protection can't drift between them.
#[allow(clippy::too_many_arguments)]
pub fn compute_token_out_min<C: CustomQuery>(
    querier: &QuerierWrapper<C>,
    pool_id: u64,
    token_in: &Coin,
    ask_denom: &str,
    belief_price: Option<Decimal>,
    max_spread: Option<Decimal>,
    allow_high_max_spread: Option<bool>,
) -> Result<Uint128, ContractError> {
    let estimated_out = estimate_swap_out(querier, pool_id, token_in, ask_denom);
    let token_out_min = derive_token_out_min(
        token_in.amount,
        estimated_out,
        belief_price,
        max_spread,
        allow_high_max_spread,
    )?;
    // CARRY-OVER 2 — reject a zero slippage floor. `derive_token_out_min`
    // stays fail-soft (a failed estimate query contributes a zero estimate
    // floor, and a caller may legitimately pass no `belief_price`), but the
    // COMBINATION of `estimated_out == 0` AND no `belief_price` collapses the
    // floor to zero, which would dispatch `MsgSwapExactAmountIn` with
    // `token_out_min_amount = 0` — an unprotected swap with no
    // sandwich/slippage guard. Both swap sites route through this helper, so
    // rejecting here closes that residual at a single choke point. On a
    // functioning Osmosis pool the estimate always resolves non-zero, so this
    // only fires when the estimate is genuinely unavailable and the caller
    // supplied no belief price.
    if token_out_min.is_zero() {
        return Err(ContractError::MaxSpreadAssertion {});
    }
    Ok(token_out_min)
}

/// FIX G — native relative circuit breaker.
///
/// Queries the LIVE per-side liquidity of the native GAMM pool (`pool_id`)
/// via the poolmanager `total_pool_liquidity` query and compares each side
/// against the amount seeded at threshold-crossing ([`SEED_LIQUIDITY`]). If
/// EITHER side has fallen below [`BREAKER_FLOOR_PERCENT`]% of its seeded
/// amount, the pool is auto-paused (`POOL_PAUSED` + `POOL_PAUSED_AUTO` set
/// to `true`) and the current call is rejected with
/// [`ContractError::PoolPausedLowLiquidity`]. Manual admin `Unpause` clears
/// both flags. Replaces the retired absolute `MINIMUM_LIQUIDITY` guard,
/// which is meaningless on a native pool.
///
/// Called at the START of swap routing on BOTH swap sites (SimpleSwap here,
/// and the post-threshold commit path), before dispatching the swap.
///
/// Two fail-soft short-circuits (return `Ok`, breaker not applied):
/// - `SEED_LIQUIDITY` unset — a pre-breaker or pre-threshold pool has no
///   snapshot to compare against.
/// - the `total_pool_liquidity` query errors — a transient/unavailable query
///   must not brick every swap; the per-swap `token_out_min_amount` floor
///   still protects the individual trade.
///
/// A side missing entirely from the query response reads as zero liquidity
/// and therefore trips the breaker — the correct, conservative behaviour if
/// the pool has been drained of one side.
pub fn enforce_liquidity_breaker<C: CustomQuery>(
    storage: &mut dyn Storage,
    querier: &QuerierWrapper<C>,
    pool_id: u64,
    bluechip_denom: &str,
    creator_denom: &str,
) -> Result<(), ContractError> {
    let (seed_osmo, seed_creator) = match SEED_LIQUIDITY.may_load(storage)? {
        Some(seed) => seed,
        None => return Ok(()),
    };

    let liquidity = match PoolmanagerQuerier::new(querier).total_pool_liquidity(pool_id) {
        Ok(resp) => resp.liquidity,
        Err(_) => return Ok(()),
    };

    // Resolve each side's current amount by denom; a side absent from the
    // response reads as zero (drained), which trips the breaker.
    let current_of = |denom: &str| -> Uint128 {
        liquidity
            .iter()
            .find(|c| c.denom == denom)
            .and_then(|c| Uint128::from_str(&c.amount).ok())
            .unwrap_or_default()
    };
    let current_osmo = current_of(bluechip_denom);
    let current_creator = current_of(creator_denom);

    // `current < BREAKER_FLOOR_PERCENT% of seed`  ⇔
    // `current * 100 < seed * BREAKER_FLOOR_PERCENT`.
    // `saturating_mul` keeps the comparison well-defined for huge balances:
    // a saturated `current * 100` is enormous, so a healthy side never trips.
    let floor_pct = Uint128::new(BREAKER_FLOOR_PERCENT);
    let hundred = Uint128::new(100);
    let below_floor = |current: Uint128, seed: Uint128| -> bool {
        current.saturating_mul(hundred) < seed.saturating_mul(floor_pct)
    };

    if below_floor(current_osmo, seed_osmo) || below_floor(current_creator, seed_creator) {
        POOL_PAUSED.save(storage, &true)?;
        POOL_PAUSED_AUTO.save(storage, &true)?;
        return Err(ContractError::PoolPausedLowLiquidity {});
    }
    Ok(())
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

    // FIX G — trip the native relative circuit breaker BEFORE dispatching
    // the swap: if either side of the live pool has fallen below
    // BREAKER_FLOOR_PERCENT% of its seeded liquidity, this auto-pauses the
    // pool and rejects. Shares the exact helper with the post-threshold
    // commit swap path so the protection can't drift between the two sites.
    // The breaker keys on the CANONICAL pair sides ([0] = bluechip Native,
    // [1] = creator) so it matches `SEED_LIQUIDITY = (seed_osmo, seed_creator)`
    // regardless of the swap DIRECTION (`offer_denom`/`ask_denom` flip on a
    // sell).
    enforce_liquidity_breaker(
        deps.storage,
        &deps.querier,
        pool_id,
        &denom_of(&pool_info.pool_info.asset_infos[0]),
        &denom_of(&pool_info.pool_info.asset_infos[1]),
    )?;

    let token_in = Coin {
        denom: offer_denom.clone(),
        amount: offer_asset.amount,
    };

    // Slippage floor = max(on-chain-estimate floor, belief-price floor).
    // The estimate is queried against the live native pool at `pool_id`;
    // both floors share the same shared helper as the post-threshold
    // commit swap so protection can't drift between the two sites.
    let token_out_min_amount = compute_token_out_min(
        &deps.querier,
        pool_id,
        &token_in,
        &ask_denom,
        belief_price,
        max_spread,
        allow_high_max_spread,
    )?;
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
