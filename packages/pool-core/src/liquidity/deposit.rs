//! First-time LP deposit handler + the shared deposit plumbing used by
//! `add_to_position` too.
//!
//! `prepare_deposit` runs the checks common to any liquidity-in
//! operation: ratio matching, slippage bounds, per-asset fund
//! collection, and returns a `DepositPrep` bundle for the caller.
//! Post-migration BOTH pool sides are native bank denoms (bluechip +
//! the creator TokenFactory denom), so every side is collected the same
//! way: verify attached `info.funds` and BankMsg-refund any overpayment.
//! `execute_deposit_liquidity` then uses that bundle to mint a fresh
//! position NFT + credit the LP.
//!
//! Both `DepositPrep` and `prepare_deposit` are `pub(crate)` so
//! `super::add::add_to_position` can reuse them without re-implementing
//! the collection logic.

use cosmwasm_std::{
    to_json_binary, Addr, BankMsg, Coin, CosmosMsg, Deps, DepsMut, Env, MessageInfo, Response,
    StdError, StdResult, SubMsg, Timestamp, Uint128, WasmMsg,
};
use pool_factory_interfaces::cw721_msgs::{Action, Cw721ExecuteMsg};

use crate::asset::TokenType;
use crate::error::ContractError;
use crate::generic::{
    check_liquidity_rate_limit, enforce_transaction_deadline, with_reentrancy_guard,
};
use crate::liquidity_helpers::{
    calc_liquidity_for_deposit, calculate_fee_size_multiplier, check_slippage,
};
use crate::state::{
    DepositVerifyContext, PoolInfo, PoolSpecs, PoolState, Position, TokenMetadata,
    DEPOSIT_VERIFY_CTX, DEPOSIT_VERIFY_REPLY_ID, LIQUIDITY_POSITIONS, MINIMUM_LIQUIDITY,
    NEXT_POSITION_ID, OWNER_POSITIONS, POOL_ANALYTICS, POOL_FEE_STATE, POOL_INFO, POOL_PAUSED,
    POOL_PAUSED_AUTO, POOL_SPECS, POOL_STATE,
};
use crate::swap::update_price_accumulator;

/// Everything prepare_deposit discovers up front: how much liquidity the
/// deposit will produce, how much of each side was actually used (vs the
/// offered `amount0`/`amount1` — ratio-matching may clamp one side), and
/// the exact list of CosmosMsgs needed to move tokens into position
/// (TransferFrom for CW20 sides, BankMsg refunds for over-paid native
/// sides).
///
/// `collect_msgs` is pair-shape agnostic: for each of the two asset
/// positions we emit the appropriate overpayment-refund message. Both
/// sides are native bank denoms now, so the list is at most two BankMsg
/// refunds (one per over-paid side).
pub(crate) struct DepositPrep {
    pub pool_info: PoolInfo,
    /// POOL_STATE as loaded once for the deposit-amount math. Nothing
    /// writes POOL_STATE between `prepare_deposit` and the handlers'
    /// mutation phase (only balance queries run in between), so the
    /// handlers reuse this copy instead of re-reading the same item.
    pub pool_state: PoolState,
    pub liquidity: Uint128,
    pub actual_amount0: Uint128,
    pub actual_amount1: Uint128,
    pub collect_msgs: Vec<CosmosMsg>,
    /// Over-payment on the asset-0 side (always 0 unless asset 0 is
    /// `Native` AND the caller attached more of that denom than
    /// `actual_amount0` needed). Preserved for the `refunded_amount0`
    /// response attribute existing external tooling (logs, tests) parses.
    pub refund_amount0: Uint128,
    /// Same for asset-1.
    pub refund_amount1: Uint128,
}

/// For a single asset position, emit the CosmosMsgs needed to pull
/// `amount` into the pool contract and return the over-payment refund.
///
/// Post-migration BOTH sides are native bank denoms — the bluechip side
/// AND the creator TokenFactory denom — so both use the identical
/// attached-funds path: verify `info.funds` covers at least `amount` of
/// the denom; emit a BankMsg refund for the overpayment (if any) back to
/// the sender; return the refunded amount. (Pre-migration the
/// `CreatorToken` arm pulled a CW20 via `Cw20ExecuteMsg::TransferFrom`;
/// now the depositor ATTACHES the creator denom just like the bluechip
/// side.) `pool_contract` is no longer needed for the (retired) CW20
/// pull path but is kept in the signature for call-site stability.
fn collect_deposit_side(
    asset_info: &TokenType,
    amount: Uint128,
    info: &MessageInfo,
    _pool_contract: &Addr,
    out_msgs: &mut Vec<CosmosMsg>,
) -> Result<Uint128, ContractError> {
    let denom = match asset_info {
        TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom,
    };
    let paid = info
        .funds
        .iter()
        .find(|c| c.denom == *denom)
        .map(|c| c.amount)
        .unwrap_or_default();
    if paid < amount {
        return Err(ContractError::InvalidNativeAmount {
            expected: amount,
            actual: paid,
        });
    }
    let refund = paid.checked_sub(amount).unwrap_or(Uint128::zero());
    if !refund.is_zero() {
        out_msgs.push(CosmosMsg::Bank(BankMsg::Send {
            to_address: info.sender.to_string(),
            amount: vec![Coin {
                denom: denom.clone(),
                amount: refund,
            }],
        }));
    }
    Ok(refund)
}

pub(crate) fn prepare_deposit(
    deps: Deps,
    info: &MessageInfo,
    amount0: Uint128,
    amount1: Uint128,
    min_amount0: Option<Uint128>,
    min_amount1: Option<Uint128>,
) -> Result<DepositPrep, ContractError> {
    // RATIO-DRIFT NOTE for callers: when the pool already has reserves,
    // `calc_liquidity_for_deposit` ratio-matches the smaller side to the
    // pool's current ratio and refunds the excess on the other side
    // (Native sides only — CW20 sides simply pull less via TransferFrom).
    // If the pool ratio shifts between when the caller computed
    // `amount0`/`amount1` and when this handler runs, the actual consumed
    // amounts can deviate substantially from what the caller intended.
    //
    // Callers that care about the deposit shape MUST pass `min_amount0`
    // / `min_amount1`. The slippage gates below
    // (`check_slippage(actual_amount0, min_amount0, ...)`) reject any
    // ratio drift that would clamp the actual deposit below those floors,
    // so a mempool-delayed deposit against a now-shifted ratio fails
    // loudly instead of silently consuming a tiny fraction of the
    // offered amounts. Calls that omit both `min_amount`s accept arbitrary
    // ratio drift (used by the threshold-crossing seed and a handful of
    // test fixtures).
    let pool_info = POOL_INFO.load(deps.storage)?;

    // Reject any attached coin whose denom isn't one of the pool's
    // configured native sides. Without this gate, `collect_deposit_side`
    // would read only the matching denom out of `info.funds` and silently
    // leave any extras (e.g. accidentally-attached gas tokens, IBC denoms,
    // tokenfactory tokens) in the pool's bank balance — orphaned forever
    // because no handler emits outgoing transfers in those denoms.
    //
    // The valid set is every bank denom in the pool's `asset_infos`.
    // Post-migration both the bluechip `Native` side AND the creator
    // `CreatorToken` (TokenFactory) side are bank denoms attached as
    // funds, so BOTH contribute — the creator pool now has two valid
    // attach denoms just like a native/native pool.
    let valid_denoms: Vec<&str> = pool_info
        .pool_info
        .asset_infos
        .iter()
        .map(|ai| match ai {
            TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom.as_str(),
        })
        .collect();
    if let Some(extra) = info
        .funds
        .iter()
        .find(|c| !valid_denoms.iter().any(|d| *d == c.denom))
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "Unexpected funds: denom \"{}\" is not one of this pool's native asset denoms \
             ({:?}). Attached funds in non-pool denoms would be orphaned in the pool's \
             bank balance with no withdrawal path. Resubmit attaching only the pool's \
             configured native denom(s).",
            extra.denom, valid_denoms
        ))));
    }

    let pool_state = POOL_STATE.load(deps.storage)?;
    let (liquidity, actual_amount0, actual_amount1) =
        calc_liquidity_for_deposit(&pool_state, amount0, amount1)?;

    check_slippage(actual_amount0, min_amount0, "asset0")?;
    check_slippage(actual_amount1, min_amount1, "asset1")?;

    let mut collect_msgs: Vec<CosmosMsg> = Vec::with_capacity(2);
    let refund_amount0 = collect_deposit_side(
        &pool_info.pool_info.asset_infos[0],
        actual_amount0,
        info,
        &pool_info.pool_info.contract_addr,
        &mut collect_msgs,
    )?;
    let refund_amount1 = collect_deposit_side(
        &pool_info.pool_info.asset_infos[1],
        actual_amount1,
        info,
        &pool_info.pool_info.contract_addr,
        &mut collect_msgs,
    )?;

    Ok(DepositPrep {
        pool_info,
        pool_state,
        liquidity,
        actual_amount0,
        actual_amount1,
        collect_msgs,
        refund_amount0,
        refund_amount1,
    })
}

/// Public deposit entry point without balance verification. Passes
/// `verify_balances = false` to skip the SubMsg verification — suitable
/// only when every CW20 side is fully trusted (no transfer fee, no
/// rebase), e.g. a factory-minted `cw20-base` token.
#[allow(clippy::too_many_arguments)]
pub fn execute_deposit_liquidity(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: Addr,
    amount0: Uint128,
    amount1: Uint128,
    min_amount0: Option<Uint128>,
    min_amount1: Option<Uint128>,
    transaction_deadline: Option<Timestamp>,
) -> Result<Response, ContractError> {
    execute_deposit_liquidity_dispatch(
        deps,
        env,
        info,
        user,
        amount0,
        amount1,
        min_amount0,
        min_amount1,
        transaction_deadline,
        false,
    )
}

/// Balance-verifying variant — used by the creator pool as
/// defense-in-depth against any future third-party CW20 integration.
/// Snapshots the pool's pre-balance
/// for every CW20 side, dispatches the final outgoing message as a
/// `SubMsg::reply_on_success`, and lets the contract's `reply` entry
/// point call `crate::balance_verify::handle_deposit_verify_reply` to
/// confirm the post-balance delta matches the credited amount. A
/// shortfall (fee-on-transfer / negative-rebase CW20) propagates an
/// `Err` from the reply, rolling the entire transaction back so the
/// pool's reserves never drift away from its on-chain balances.
#[allow(clippy::too_many_arguments)]
pub fn execute_deposit_liquidity_with_verify(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: Addr,
    amount0: Uint128,
    amount1: Uint128,
    min_amount0: Option<Uint128>,
    min_amount1: Option<Uint128>,
    transaction_deadline: Option<Timestamp>,
) -> Result<Response, ContractError> {
    execute_deposit_liquidity_dispatch(
        deps,
        env,
        info,
        user,
        amount0,
        amount1,
        min_amount0,
        min_amount1,
        transaction_deadline,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_deposit_liquidity_dispatch(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: Addr,
    amount0: Uint128,
    amount1: Uint128,
    min_amount0: Option<Uint128>,
    min_amount1: Option<Uint128>,
    transaction_deadline: Option<Timestamp>,
    verify_balances: bool,
) -> Result<Response, ContractError> {
    enforce_transaction_deadline(env.block.time, transaction_deadline)?;

    // Shared reentrancy guard across commit + swap + every liquidity
    // path. A hostile CW20 (not the factory-minted cw20-base, but any
    // future third-party token) could otherwise re-enter the pool
    // during an outgoing TransferFrom call and observe / mutate stale
    // state. Routed through the `with_reentrancy_guard` helper for the
    // same lock-clear-on-both-paths invariant the other entry points
    // use.
    with_reentrancy_guard(deps, move |mut deps| {
        // Per-user rate limit; matches `add_to_position` / `remove_*`
        // paths. Without it, a user can open unlimited Position NFTs in
        // one block (NFT-id namespace bloat + extra surface for
        // atomic-exploit chains). Rate-limit Err propagates out of the
        // closure; the helper still clears the lock on the way back.
        let pool_specs: PoolSpecs = POOL_SPECS.load(deps.storage)?;
        check_liquidity_rate_limit(&mut deps, &env, &pool_specs, &info.sender)?;
        execute_deposit_liquidity_inner(
            deps,
            env,
            info,
            user,
            amount0,
            amount1,
            min_amount0,
            min_amount1,
            verify_balances,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_deposit_liquidity_inner(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    user: Addr,
    amount0: Uint128,
    amount1: Uint128,
    min_amount0: Option<Uint128>,
    min_amount1: Option<Uint128>,
    verify_balances: bool,
) -> Result<Response, ContractError> {
    let mut prep = prepare_deposit(
        deps.as_ref(),
        &info,
        amount0,
        amount1,
        min_amount0,
        min_amount1,
    )?;

    // Snapshot the pool's current CW20 balance on every CW20 side
    // BEFORE the TransferFrom messages dispatch. The reply handler will
    // diff post-balance against this snapshot and reject any shortfall
    // (fee-on-transfer / negative-rebase). Native sides return None
    // (bank transfers are exact, no verification needed).
    //
    // For verify=false we skip the queries entirely — only safe when
    // the CW20 can never charge a transfer fee or rebase, making the
    // verification a guaranteed no-op.
    let pre_snapshot = if verify_balances {
        Some(snapshot_pool_cw20_balances(
            deps.as_ref(),
            &prep.pool_info.pool_info.contract_addr,
            &prep.pool_info.pool_info.asset_infos,
        )?)
    } else {
        None
    };

    // Reuse the POOL_STATE copy loaded in `prepare_deposit` — nothing
    // has written it since (the snapshot step above only queries).
    let mut pool_state = prep.pool_state.clone();
    let pool_fee_state = POOL_FEE_STATE.load(deps.storage)?;

    // First-depositor detection. `total_liquidity == 0` AND both reserves
    // were zero immediately before this call → genuinely empty pool, this
    // is the inflation-attack-relevant first deposit. We lock
    // `MINIMUM_LIQUIDITY` LP units of the depositor's Position so they
    // can never withdraw the principal, while letting fees still accrue
    // against the full position (see `Position.locked_liquidity` doc).
    //
    // Creator pools after threshold crossing have non-zero seed reserves
    // and non-zero `total_liquidity`, so this branch is not taken there
    // — first post-threshold LPs deposit normally with no lock.
    let is_first_deposit = pool_state.total_liquidity.is_zero()
        && pool_state.reserve0.is_zero()
        && pool_state.reserve1.is_zero();
    let locked_liquidity = if is_first_deposit {
        MINIMUM_LIQUIDITY
    } else {
        Uint128::zero()
    };

    // Max 4 outgoing messages on this path: optional NFT-accept (1) +
    // at-most-two collect msgs from prepare_deposit (2) + position-mint
    // NFT (1). Sizing once here avoids the 1-2 reallocations a default
    // Vec::new() would take as we push past its initial capacity.
    let mut messages: Vec<CosmosMsg> = Vec::with_capacity(4);
    if !pool_state.nft_ownership_accepted {
        let accept_msg = WasmMsg::Execute {
            contract_addr: prep.pool_info.position_nft_address.to_string(),
            msg: to_json_binary(&Cw721ExecuteMsg::<()>::UpdateOwnership(
                Action::AcceptOwnership,
            ))?,
            funds: vec![],
        };
        messages.push(CosmosMsg::Wasm(accept_msg));
        pool_state.nft_ownership_accepted = true;
    }

    // prepare_deposit already dispatched per-asset and built the collection
    // messages (TransferFrom for CW20 sides, BankMsg refunds for over-paid
    // native sides). Moved (not cloned) into the response list — `prep`'s
    // remaining fields are all `Copy` so the partial move is fine.
    messages.append(&mut prep.collect_msgs);

    let mut pos_id = NEXT_POSITION_ID.load(deps.storage)?;
    pos_id = pos_id
        .checked_add(1)
        .ok_or_else(|| ContractError::Std(StdError::generic_err("Position ID overflow")))?;
    NEXT_POSITION_ID.save(deps.storage, &pos_id)?;
    let position_id = pos_id.to_string();

    let metadata = TokenMetadata {
        name: Some(format!("LP Position #{}", position_id)),
        description: Some("Pool Liquidity Position".to_string()),
    };
    let mint_liquidity_nft = WasmMsg::Execute {
        contract_addr: prep.pool_info.position_nft_address.to_string(),
        msg: to_json_binary(&Cw721ExecuteMsg::<TokenMetadata>::Mint {
            token_id: position_id.clone(),
            owner: user.to_string(),
            token_uri: None,
            extension: metadata,
        })?,
        funds: vec![],
    };
    messages.push(CosmosMsg::Wasm(mint_liquidity_nft));
    let fee_size_multiplier = calculate_fee_size_multiplier(prep.liquidity);
    let position = Position {
        liquidity: prep.liquidity,
        owner: user.clone(),
        fee_growth_inside_0_last: pool_fee_state.fee_growth_global_0,
        fee_growth_inside_1_last: pool_fee_state.fee_growth_global_1,
        created_at: env.block.time.seconds(),
        last_fee_collection: env.block.time.seconds(),
        fee_size_multiplier,
        unclaimed_fees_0: Uint128::zero(),
        unclaimed_fees_1: Uint128::zero(),
        locked_liquidity,
    };

    LIQUIDITY_POSITIONS.save(deps.storage, &position_id, &position)?;
    OWNER_POSITIONS.save(deps.storage, (&user, &position_id), &true)?;

    pool_state.reserve0 = pool_state.reserve0.checked_add(prep.actual_amount0)?;
    pool_state.reserve1 = pool_state.reserve1.checked_add(prep.actual_amount1)?;
    pool_state.total_liquidity = pool_state.total_liquidity.checked_add(prep.liquidity)?;
    update_price_accumulator(&mut pool_state, env.block.time.seconds())?;
    POOL_STATE.save(deps.storage, &pool_state)?;

    // Auto-unpause when a deposit restores reserves above MIN AND
    // the pool was auto-paused (POOL_PAUSED_AUTO == true). Admin pauses
    // and emergency-pending pauses (POOL_PAUSED_AUTO == false) are NOT
    // cleared here — those require explicit admin Unpause / cancel.
    //
    // Today this branch is reachable only via the recovery path: the
    // deposit dispatch's `check_pool_writable_for_deposit` permits
    // entering the handler when `pause_kind == AutoLowLiquidity`.
    // Hard-paused pools never reach this code (rejected at dispatch),
    // so the auto-flag check here is the second layer of the same
    // invariant — defense-in-depth against any future call site that
    // bypasses the dispatch gate.
    let was_auto_paused = POOL_PAUSED_AUTO.may_load(deps.storage)?.unwrap_or(false);
    let reserves_recovered =
        pool_state.reserve0 >= MINIMUM_LIQUIDITY && pool_state.reserve1 >= MINIMUM_LIQUIDITY;
    let unpaused = was_auto_paused && reserves_recovered;
    if unpaused {
        POOL_PAUSED.save(deps.storage, &false)?;
        POOL_PAUSED_AUTO.save(deps.storage, &false)?;
    }

    // Update analytics
    let mut analytics = POOL_ANALYTICS.may_load(deps.storage)?.unwrap_or_default();
    analytics.total_lp_deposit_count += 1;
    POOL_ANALYTICS.save(deps.storage, &analytics)?;

    // Share of pool in basis points
    let share_of_pool_bps = if !pool_state.total_liquidity.is_zero() {
        prep.liquidity
            .checked_mul(Uint128::from(10000u128))
            .unwrap_or(Uint128::MAX)
            .checked_div(pool_state.total_liquidity)
            .unwrap_or(Uint128::zero())
            .to_string()
    } else {
        "10000".to_string() // 100% if first depositor
    };

    let attrs = vec![
        ("action", "deposit_liquidity".to_string()),
        ("position_id", position_id),
        ("depositor", user.to_string()),
        ("liquidity", prep.liquidity.to_string()),
        ("actual_amount0", prep.actual_amount0.to_string()),
        ("actual_amount1", prep.actual_amount1.to_string()),
        ("refunded_amount0", prep.refund_amount0.to_string()),
        ("refunded_amount1", prep.refund_amount1.to_string()),
        ("offered_amount0", amount0.to_string()),
        ("offered_amount1", amount1.to_string()),
        ("reserve0_after", pool_state.reserve0.to_string()),
        ("reserve1_after", pool_state.reserve1.to_string()),
        (
            "total_liquidity_after",
            pool_state.total_liquidity.to_string(),
        ),
        ("share_of_pool_bps", share_of_pool_bps),
        (
            "pool_contract",
            pool_state.pool_contract_address.to_string(),
        ),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
        (
            "total_lp_deposit_count",
            analytics.total_lp_deposit_count.to_string(),
        ),
        (
            "pool_unpaused",
            if unpaused {
                "true".to_string()
            } else {
                "false".to_string()
            },
        ),
    ];

    finalize_deposit_response(
        deps.storage,
        &prep.pool_info,
        &prep.pool_info.pool_info.asset_infos,
        prep.actual_amount0,
        prep.actual_amount1,
        // Fresh deposit emits no in-tx CW20 outflows (no fee payout —
        // the position has no prior fee accrual). Pass zero outgoing
        // amounts; the verify reply's `post == pre + actual` invariant
        // simplifies cleanly.
        (Uint128::zero(), Uint128::zero()),
        pre_snapshot,
        messages,
        attrs,
    )
}

// ---------------------------------------------------------------------------
// Shared SubMsg-based deposit balance verification helpers.
//
// `pub(crate)` so `super::add::add_to_position` can reuse them on the
// add-to-position path. The reply ID + storage Item live in `state.rs`
// so any contract that wires a `reply` entry point in the future
// (creator-pool, anchor-pool, etc.) can dispatch to
// `crate::balance_verify::handle_deposit_verify_reply` without taking
// a dependency on this module.

/// Per-side pre-balance snapshot returned by `snapshot_pool_cw20_balances`.
/// `None` means the side is `TokenType::Native` and therefore not
/// verified — bank transfers are exact, no fee-on-transfer is possible.
pub(crate) type PreBalanceSnapshot = (Option<Uint128>, Option<Uint128>);

/// Queries the pool contract's current CW20 balance for every CW20 side
/// in `asset_infos`, in pair order. Returns `None` for any `Native` side.
///
/// Strict — propagates query errors. Swallowing them as zero would let
/// the post-balance query's full pool reserve appear as a "delta" and
/// silently mask exactly the fee-on-transfer corruption this
/// verification is designed to catch.
pub(crate) fn snapshot_pool_cw20_balances(
    _deps: Deps,
    _pool_addr: &Addr,
    _asset_infos: &[TokenType; 2],
) -> StdResult<PreBalanceSnapshot> {
    // Post-migration there are no CW20 sides: the bluechip side is a
    // native bank denom and the creator token is a TokenFactory native
    // bank denom. Bank transfers are exact (no fee-on-transfer / rebase
    // possible), so there is nothing to snapshot — both sides return
    // `None` and the deposit balance-verify SubMsg is never wired.
    //
    // The function and the `DepositVerifyContext` machinery are retained
    // (dormant) so re-introducing a third-party CW20 side in a later
    // phase only requires re-populating these snapshots rather than
    // re-plumbing the reply path.
    Ok((None, None))
}

/// Builds the final `Response`. When `pre_snapshot.is_none()` (creator-
/// pool / verify=false path) returns the response with plain
/// `add_messages` — no SubMsgs, no transient state, no behavior change.
///
/// When `pre_snapshot.is_some()` AND at least one side is CW20:
/// - Saves a `DepositVerifyContext` with the pre-balances + the
/// credited amounts (`actual_amount0`/`1`).
/// - Converts the LAST entry of `messages` from a fire-and-forget
/// `CosmosMsg` into a `SubMsg::reply_on_success(.., DEPOSIT_VERIFY_REPLY_ID)`.
/// CosmWasm dispatches the reply after every other message in the
/// response has processed, so by the time it fires, all
/// TransferFroms have already settled and the post-balance query
/// reflects the actual delta.
///
/// When `pre_snapshot.is_some()` BUT every side is Native (a
/// native/native pair): same as the verify=false path —
/// nothing to verify, no SubMsg conversion, no transient state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_deposit_response(
    storage: &mut dyn cosmwasm_std::Storage,
    pool_info: &PoolInfo,
    asset_infos: &[TokenType; 2],
    actual_amount0: Uint128,
    actual_amount1: Uint128,
    // Per-side CW20 amounts that flow OUT of the pool inside the same
    // Response as the inflow. Used by `add_to_position` to declare the
    // fee-payout (or other) outflows on each CW20 side; passed as
    // `(Uint128::zero(), Uint128::zero())` from the deposit path where
    // no in-tx outflows happen.
    //
    // The verify reply enforces `post + outgoing == pre + actual_amount`
    // per CW20 side so the strict equality check accounts for the net
    // pool-balance change rather than treating outflows as fee-on-transfer
    // shortfalls.
    outgoing_amounts: (Uint128, Uint128),
    pre_snapshot: Option<PreBalanceSnapshot>,
    messages: Vec<CosmosMsg>,
    attrs: Vec<(&'static str, String)>,
) -> Result<Response, ContractError> {
    let snapshot = match pre_snapshot {
        Some(s) => s,
        None => {
            return Ok(Response::new().add_messages(messages).add_attributes(attrs));
        }
    };

    // Post-migration every pool side is a native bank denom, so there is
    // no CW20 address to verify against. Both are always `None`, so the
    // native+native fast path below always returns.
    let cw20_side0_addr: Option<Addr> = match &asset_infos[0] {
        TokenType::CreatorToken { .. } | TokenType::Native { .. } => None,
    };
    let cw20_side1_addr: Option<Addr> = match &asset_infos[1] {
        TokenType::CreatorToken { .. } | TokenType::Native { .. } => None,
    };

    if cw20_side0_addr.is_none() && cw20_side1_addr.is_none() {
        // All-native shape: nothing to verify.
        return Ok(Response::new().add_messages(messages).add_attributes(attrs));
    }

    // messages is non-empty here: every successful deposit emits at
    // minimum the position-NFT mint message (and typically a CW20
    // TransferFrom alongside). Defensive check just in case a future
    // refactor produces an empty list.
    if messages.is_empty() {
        return Err(ContractError::Std(StdError::generic_err(
            "cannot wire deposit balance verification on an empty \
             outgoing message list",
        )));
    }

    DEPOSIT_VERIFY_CTX.save(
        storage,
        &DepositVerifyContext {
            pool_addr: pool_info.pool_info.contract_addr.clone(),
            cw20_side0_addr,
            cw20_side1_addr,
            pre_balance0: snapshot.0.unwrap_or_default(),
            pre_balance1: snapshot.1.unwrap_or_default(),
            expected_delta0: actual_amount0,
            expected_delta1: actual_amount1,
            outgoing_amount0: outgoing_amounts.0,
            outgoing_amount1: outgoing_amounts.1,
        },
    )?;

    // Convert the last CosmosMsg into a reply_on_success SubMsg; everything
    // else stays as fire-and-forget.
    let mut sub_msgs: Vec<SubMsg> = messages.into_iter().map(SubMsg::new).collect();
    let last_idx = sub_msgs.len() - 1;
    sub_msgs[last_idx] =
        SubMsg::reply_on_success(sub_msgs[last_idx].msg.clone(), DEPOSIT_VERIFY_REPLY_ID);

    Ok(Response::new()
        .add_submessages(sub_msgs)
        .add_attributes(attrs))
}
