//! Multi-hop execution logic.
//!
//! ## Execution model
//!
//! The router uses the standard CosmWasm self-recursion pattern. The
//! public [`execute_multi_hop`] (or [`execute_receive_cw20`]) entry
//! point validates the route, captures the recipient's pre-route balance
//! of the final ask token, and then builds a `Response` whose messages
//! are a sequence of self-calls:
//!
//! 1. One [`crate::msg::ExecuteMsg::ExecuteSwapOperation`] per hop, in
//! order. Hops 0..N-1 send their output back to the router; hop N
//! sends its output directly to the recipient. Each is wrapped in a
//! `SubMsg::reply_on_error` so the [`handle_reply`] handler can
//! re-raise raw pool errors as [`crate::error::RouterError::HopFailed`]
//! with hop context preserved through the submsg payload.
//!
//! 2. One trailing [`crate::msg::ExecuteMsg::AssertReceived`] self-call
//! that compares the recipient's post-route balance to the captured
//! pre-route balance and rejects if the delta is below
//! `minimum_receive`.
//!
//! Atomicity comes for free: every message in a `Response` runs in
//! sequence within a single transaction; any error reverts everything.
//!
//! ## Why per-hop balance reads are safe
//!
//! Each [`execute_swap_operation`] call swaps only `current_balance -
//! offer_baseline`, where `offer_baseline` is the router's PRE-route balance
//! of that hop's offer denom, snapshotted in [`start_multi_hop`] before any
//! funds move (M-03). So each hop consumes exactly the funds THIS route
//! produced — the attached input on hop 0, the prior hop's output on later
//! hops — and any pre-existing or donated balance sits below the baseline
//! and is left untouched. A stray deposit to the router is therefore NOT
//! swept into the next user's route; it is excluded by the baseline, not
//! captured.
//!
//! As defense-in-depth (so the safety does not rest solely on that
//! arithmetic being correct), [`execute_multi_hop`] also sets a transient
//! `ROUTE_IN_PROGRESS` guard for the duration of a route, rejecting any
//! re-entrant `ExecuteMultiHop` a malicious pool might trigger mid-hop. The
//! guard is cleared by the terminal `AssertReceived` step on success, or
//! rolled back with the whole tx on any hop failure.

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Binary, Coin, CosmosMsg, Decimal, Deps, DepsMut, Env,
    MessageInfo, Reply, ReplyOn, Response, StdError, SubMsg, SubMsgResult, Timestamp, Uint128,
    WasmMsg,
};
use pool_factory_interfaces::asset::{TokenInfo, TokenType};
use pool_factory_interfaces::routing::{
    FactoryRouteQueryMsg, PoolSwapExecuteMsg, PoolSwapQueryMsg, RouterPoolCommitStatus,
    SwapOperation,
};
use pool_factory_interfaces::RegisteredPoolResponse;

use crate::error::RouterError;
use crate::msg::ExecuteMsg;
use crate::state::{CONFIG, MAX_HOPS};

/// Reply IDs are offset by this base so that future router features can
/// claim a different range without colliding with hop replies.
pub const REPLY_ID_HOP_BASE: u64 = 1000;

/// Carried in each hop's submessage payload so that [`handle_reply`] can
/// produce a [`RouterError::HopFailed`] with the failing pool address
/// even though the reply handler does not see the original operation.
#[cw_serde]
struct HopReplyPayload {
    hop_index: u32,
    pool_addr: String,
}

/// Public entry: native offer for the first hop. The creator token is a
/// TokenFactory bank denom now, so BOTH sides (bluechip and creator) are
/// offered the same way — the caller attaches exactly one coin matching
/// the first hop's declared offer denom, and that coin's amount is the
/// route input. (Pre-migration a creator-token first hop had to arrive
/// via a CW20 `Send` through a now-removed `execute_receive_cw20` entry.)
pub fn execute_multi_hop(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    operations: Vec<SwapOperation>,
    minimum_receive: Uint128,
    deadline: Option<Timestamp>,
    recipient: Option<String>,
) -> Result<Response, RouterError> {
    let first_op = operations.first().ok_or(RouterError::EmptyRoute)?;
    // Both variants are native bank denoms — attached as funds and
    // extracted identically.
    let offer_amount = match &first_op.offer_asset_info {
        TokenType::Native { denom } | TokenType::CreatorToken { denom } => {
            extract_native_offer(&info, denom)?
        }
    };

    start_multi_hop(
        deps,
        env,
        info.sender,
        offer_amount,
        operations,
        minimum_receive,
        deadline,
        recipient,
    )
}

/// Shared route setup. Validates the route, captures the recipient's
/// pre-route balance of the final ask token, and builds the per-hop
/// self-call sequence plus the final assertion call.
//
// Eight parameters: this is the funnel both the native and CW20 entry
// points feed after unpacking their wire messages — each argument is one
// already-validated field, and bundling them into a struct would just
// re-create the wire message under another name.
#[allow(clippy::too_many_arguments)]
fn start_multi_hop(
    deps: DepsMut,
    env: Env,
    sender: Addr,
    offer_amount: Uint128,
    operations: Vec<SwapOperation>,
    minimum_receive: Uint128,
    deadline: Option<Timestamp>,
    recipient: Option<String>,
) -> Result<Response, RouterError> {
    if offer_amount.is_zero() {
        return Err(RouterError::ZeroAmount);
    }
    // F-5 — reentrancy guard. A route in progress means a pool called during
    // one of this route's hops is trying to re-enter with a nested route;
    // reject it. Set here and cleared by the terminal `AssertReceived`. The
    // sub-message writes of a route are visible to a reentrant call within the
    // same tx, so the flag reliably blocks nesting; on any failure the tx
    // reverts and the flag rolls back to unset.
    if crate::state::ROUTE_IN_PROGRESS
        .may_load(deps.storage)?
        .unwrap_or(false)
    {
        return Err(RouterError::Reentrancy);
    }
    crate::state::ROUTE_IN_PROGRESS.save(deps.storage, &true)?;
    // With per-hop max_spread pinned to the pools' 5% hard cap,
    // minimum_receive is the ONLY end-to-end slippage guard. Zero means a
    // 3-hop route could be sandwiched for up to ~14% with no recourse; no
    // retail flow ever wants that, and frontends size it from
    // SimulateMultiHop. Fail closed at the shared entry point (covers both
    // the native and CW20 paths).
    if minimum_receive.is_zero() {
        return Err(RouterError::ZeroMinimumReceive);
    }
    if let Some(d) = deadline {
        if env.block.time > d {
            return Err(RouterError::DeadlineExceeded {
                deadline: d.seconds(),
                current: env.block.time.seconds(),
            });
        }
    }
    validate_route(&operations)?;

    // Validate every hop's pool address against the factory registry BEFORE
    // any funds move. `validate_route` only checks the route's internal
    // consistency (shape, continuity, hop count) — it cannot tell a genuine
    // pool from an arbitrary caller-supplied contract. Without this step the
    // router would forward the user's offer to whatever address the
    // (possibly malicious) frontend placed in `pool_addr`, with
    // `minimum_receive` as the only backstop. Querying the factory makes the
    // stored `factory_addr` load-bearing and bounds a hostile route to
    // genuine, registered pools.
    let factory_addr = CONFIG.load(deps.storage)?.factory_addr;
    validate_route_pools_registered(deps.as_ref(), &factory_addr, &operations)?;

    let recipient_addr = match recipient {
        Some(r) => deps.api.addr_validate(&r)?,
        None => sender.clone(),
    };

    let final_ask = operations.last().unwrap().ask_asset_info.clone();
    // Strict query: on a CW20 final-ask, a swallowed
    // pre-balance query would silently report zero and let the recipient's
    // pre-existing CW20 holdings count toward the post-route "received"
    // total — eroding slippage protection by up to that amount. The strict
    // variant propagates query errors so a failed CW20 read fails the
    // entire route closed instead of corrupting the assertion.
    let recipient_initial_balance =
        final_ask.query_pool_strict(&deps.querier, recipient_addr.clone())?;

    let last_idx = operations.len() - 1;
    let mut messages: Vec<SubMsg> = Vec::with_capacity(operations.len() + 1);

    // M-03 — snapshot the router's PRE-route balance of each hop's offer
    // denom so each hop swaps only the funds THIS route produces, never a
    // pre-existing/donated balance. For the FIRST hop's offer denom the
    // snapshot already includes the just-attached `offer_amount` (funds are
    // credited before `execute` runs), so subtract it back out to recover
    // the true pre-existing baseline. Every hop then swaps
    // `current_offer_balance - offer_baseline`, which equals the attached
    // input on hop 0 and the prior hop's output on later hops — even when a
    // denom repeats across non-adjacent hops, because each hop consumes its
    // full computed input and later inflows of that denom come only from
    // subsequent hops.
    let first_offer_info = &operations[0].offer_asset_info;

    for (idx, op) in operations.iter().enumerate() {
        let to = if idx == last_idx {
            recipient_addr.to_string()
        } else {
            env.contract.address.to_string()
        };
        let snapshot = op
            .offer_asset_info
            .query_pool_strict(&deps.querier, env.contract.address.clone())?;
        let offer_baseline = if op.offer_asset_info.equal(first_offer_info) {
            snapshot.saturating_sub(offer_amount)
        } else {
            snapshot
        };
        let exec_op = ExecuteMsg::ExecuteSwapOperation {
            operation: op.clone(),
            hop_index: idx as u32,
            to,
            offer_baseline,
        };
        let payload: Binary = to_json_binary(&HopReplyPayload {
            hop_index: idx as u32,
            pool_addr: op.pool_addr.clone(),
        })?;
        let sub = SubMsg::reply_on_error(
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: env.contract.address.to_string(),
                msg: to_json_binary(&exec_op)?,
                funds: vec![],
            }),
            hop_reply_id(idx as u32),
        )
        .with_payload(payload);
        messages.push(sub);
    }

    let assert_msg = ExecuteMsg::AssertReceived {
        ask_info: final_ask,
        recipient: recipient_addr.to_string(),
        prev_balance: recipient_initial_balance,
        minimum_receive,
    };
    messages.push(SubMsg {
        id: 0,
        payload: Binary::default(),
        msg: CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: env.contract.address.to_string(),
            msg: to_json_binary(&assert_msg)?,
            funds: vec![],
        }),
        gas_limit: None,
        reply_on: ReplyOn::Never,
    });

    Ok(Response::new()
        .add_submessages(messages)
        .add_attribute("action", "execute_multi_hop")
        .add_attribute("sender", sender)
        .add_attribute("recipient", recipient_addr)
        .add_attribute("offer_amount", offer_amount)
        .add_attribute("hops", operations.len().to_string())
        .add_attribute("minimum_receive", minimum_receive))
}

/// Internal handler for one hop. Self-only.
///
/// Reads the router's current balance of the offer token (which equals
/// either the user's deposit on hop 0 or the previous hop's output on
/// hops 1..N), then dispatches the underlying pool swap targeting `to`.
///
/// The underlying pool message is built with `belief_price = None` and
/// `max_spread = None` unconditionally — see the module-level
/// `ExecuteMsg` doc-comment in `msg.rs` for why per-hop slippage knobs
/// are not exposed at the multi-hop level (`minimum_receive` is the
/// canonical end-to-end gate).
pub fn execute_swap_operation(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    operation: SwapOperation,
    hop_index: u32,
    to: String,
    offer_baseline: Uint128,
) -> Result<Response, RouterError> {
    if info.sender != env.contract.address {
        return Err(RouterError::Unauthorized);
    }
    let to_addr = deps.api.addr_validate(&to)?;

    // Strict query. The router's own balance is what
    // becomes the swap input for this hop; if a CW20 balance query
    // silently returns zero on error we'd dispatch a zero-amount swap
    // and the explicit zero-check below would mask the underlying
    // query failure. Strict propagation surfaces the real cause.
    let offer_balance = operation
        .offer_asset_info
        .query_pool_strict(&deps.querier, env.contract.address.clone())?;

    // M-03 — swap only the funds THIS route produced for this hop, not the
    // router's whole balance of the offer denom. `offer_baseline` is the
    // pre-route balance snapshotted by `start_multi_hop`; the delta is the
    // attached input (hop 0) or the prior hop's output (later hops). A
    // pre-existing/donated balance sits below the baseline and is left
    // untouched. `saturating_sub` is defensive — the balance can only have
    // grown by this route's inflows since the snapshot, so it never
    // underflows in practice.
    let swap_input = offer_balance.saturating_sub(offer_baseline);
    if swap_input.is_zero() {
        return Err(RouterError::HopFailed {
            hop_index: hop_index as usize,
            pool_addr: operation.pool_addr.clone(),
            reason: "router holds zero route-generated balance of the offer token at hop start"
                .to_string(),
        });
    }

    let pool_msg = build_pool_swap_msg(&operation, swap_input, to_addr.to_string())?;

    Ok(Response::new()
        .add_message(pool_msg)
        .add_attribute("action", "execute_swap_operation")
        .add_attribute("hop_index", hop_index.to_string())
        .add_attribute("pool", operation.pool_addr)
        .add_attribute("offer_amount", swap_input)
        .add_attribute("to", to_addr))
}

/// Internal handler for the final slippage check. Self-only.
pub fn execute_assert_received(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    ask_info: TokenType,
    recipient: String,
    prev_balance: Uint128,
    minimum_receive: Uint128,
) -> Result<Response, RouterError> {
    if info.sender != env.contract.address {
        return Err(RouterError::Unauthorized);
    }
    let recipient_addr = deps.api.addr_validate(&recipient)?;
    // Strict query. Symmetric with the pre-route read in
    // `start_multi_hop` — both must use the same strict variant so a
    // CW20 query failure fails the assertion closed.
    let current_balance = ask_info.query_pool_strict(&deps.querier, recipient_addr.clone())?;
    let received = current_balance.checked_sub(prev_balance).map_err(|_| {
        RouterError::Std(StdError::generic_err(
            "recipient balance decreased during route; impossible state",
        ))
    })?;
    if received < minimum_receive {
        return Err(RouterError::SlippageExceeded {
            minimum: minimum_receive,
            actual: received,
        });
    }
    // F-5 — route completed successfully; clear the reentrancy guard. This is
    // the terminal step of every route (always appended after the hops), so
    // the flag set in `start_multi_hop` is always cleared on the success path;
    // on a failure path the tx reverts and the flag rolls back instead.
    crate::state::ROUTE_IN_PROGRESS.save(deps.storage, &false)?;
    Ok(Response::new()
        .add_attribute("action", "assert_received")
        .add_attribute("recipient", recipient_addr)
        .add_attribute("received", received)
        .add_attribute("minimum_receive", minimum_receive))
}

/// Reply handler for hop submessages. Wraps the raw pool error into a
/// [`RouterError::HopFailed`] with hop index and pool address.
///
/// The pool address is read from the submsg payload when available. Some
/// host runtimes (notably `cw-multi-test` 2.1) do not propagate the
/// payload through to replies, so the handler tolerates an empty or
/// unparseable payload by reporting an empty `pool_addr` instead of
/// failing the wrapping. The hop index is recovered from the reply ID
/// in either case so frontends always learn which hop failed.
pub fn handle_reply(_deps: DepsMut, _env: Env, msg: Reply) -> Result<Response, RouterError> {
    let hop_index = parse_hop_reply_id(msg.id).ok_or_else(|| {
        RouterError::Std(StdError::generic_err(format!(
            "unknown reply id: {}",
            msg.id
        )))
    })?;
    let pool_addr = if msg.payload.is_empty() {
        String::new()
    } else {
        from_json::<HopReplyPayload>(&msg.payload)
            .map(|p| p.pool_addr)
            .unwrap_or_default()
    };
    let reason = match msg.result {
        SubMsgResult::Err(err) => err,
        // ReplyOn::Error never fires on success; treat as a no-op so
        // the contract does not panic if a future runtime change alters
        // delivery semantics.
        SubMsgResult::Ok(_) => return Ok(Response::new()),
    };
    Err(RouterError::HopFailed {
        hop_index: hop_index as usize,
        pool_addr,
        reason,
    })
}

/// Validate every hop's `pool_addr` against the factory registry.
///
/// Chain-state counterpart to [`validate_route`] (which only checks the
/// route's internal shape). For each hop it asks the configured factory
/// whether `pool_addr` is a registered Bluechip pool and, if so, confirms
/// the hop's declared `(offer, ask)` are that pool's two real sides.
/// Rejecting here — before any funds move and atomically with the rest of
/// the route — prevents a malicious frontend from steering user funds
/// through an arbitrary contract or a real pool with a mislabeled pair.
fn validate_route_pools_registered(
    deps: Deps,
    factory_addr: &Addr,
    operations: &[SwapOperation],
) -> Result<(), RouterError> {
    for (idx, op) in operations.iter().enumerate() {
        let registered: Option<RegisteredPoolResponse> = deps.querier.query_wasm_smart(
            factory_addr,
            &FactoryRouteQueryMsg::PoolByAddress {
                pool_addr: op.pool_addr.clone(),
            },
        )?;
        let pool = registered.ok_or_else(|| RouterError::PoolNotRegistered {
            hop_index: idx,
            pool_addr: op.pool_addr.clone(),
        })?;
        // Confirm the hop's declared offer and ask are exactly the two
        // sides of the registered pool. `validate_route` already rejects
        // offer == ask, so requiring each to match a distinct registered
        // side is equivalent to set-equality with the pool's pair.
        let sides = &pool.pool_token_info;
        let offer_ok = sides.iter().any(|s| s.equal(&op.offer_asset_info));
        let ask_ok = sides.iter().any(|s| s.equal(&op.ask_asset_info));
        if !offer_ok || !ask_ok {
            return Err(RouterError::HopPairMismatch {
                hop_index: idx,
                pool_addr: op.pool_addr.clone(),
            });
        }

        // L-02 — reject a route through a pre-threshold pool up front,
        // mirroring the simulation path (`simulate_multi_hop`). A commit
        // pool that has not crossed its threshold cannot be swapped through;
        // without this check the route would still revert atomically at the
        // pool, but as an opaque wrapped `HopFailed` rather than the
        // actionable `PoolInCommitPhase` the frontend gets from simulation.
        // Checking here keeps simulate and execute in agreement.
        let commit_status: RouterPoolCommitStatus = deps.querier.query_wasm_smart(
            op.pool_addr.clone(),
            &PoolSwapQueryMsg::IsFullyCommited {},
        )?;
        if let RouterPoolCommitStatus::InProgress { raised, target } = commit_status {
            return Err(RouterError::PoolInCommitPhase {
                hop_index: idx,
                pool_addr: op.pool_addr.clone(),
                raised,
                target,
            });
        }
    }
    Ok(())
}

/// Validates a candidate route in isolation -- no chain state is read.
///
/// Performs every cheap check before any pool query so callers get fast
/// rejection of obviously malformed routes.
pub fn validate_route(operations: &[SwapOperation]) -> Result<(), RouterError> {
    if operations.is_empty() {
        return Err(RouterError::EmptyRoute);
    }
    if operations.len() > MAX_HOPS {
        return Err(RouterError::MaxHopsExceeded {
            max: MAX_HOPS,
            got: operations.len(),
        });
    }

    for (idx, op) in operations.iter().enumerate() {
        if op.offer_asset_info.equal(&op.ask_asset_info) {
            return Err(RouterError::Std(StdError::generic_err(format!(
                "hop {} declares offer and ask as the same token: {}",
                idx, op.offer_asset_info
            ))));
        }
    }

    for i in 0..operations.len() - 1 {
        let cur_ask = &operations[i].ask_asset_info;
        let next_offer = &operations[i + 1].offer_asset_info;
        if !cur_ask.equal(next_offer) {
            return Err(RouterError::RouteDiscontinuity {
                hop_index: i,
                next_hop_index: i + 1,
                transition: format!("{} -> {}", cur_ask, next_offer),
            });
        }
    }

    let input = &operations.first().unwrap().offer_asset_info;
    let output = &operations.last().unwrap().ask_asset_info;
    if input.equal(output) {
        return Err(RouterError::SameInputOutput);
    }
    Ok(())
}

/// Per-hop `max_spread` forwarded to every underlying pool call. A
/// `None` here would NOT disable the gate — pools substitute their
/// 0.5% `DEFAULT_SLIPPAGE`, which would silently fail every thin-pool
/// route regardless of the caller's `minimum_receive`. Pinning the
/// pools' 5% hard cap (the widest value accepted without
/// `allow_high_max_spread`) neutralizes the per-hop gate so
/// `minimum_receive` in `execute_assert_received` is the binding,
/// end-to-end slippage control — exactly the model the `ExecuteMsg`
/// docs promise.
fn per_hop_max_spread() -> Decimal {
    Decimal::percent(5)
}

/// Builds the underlying pool swap message for one hop. `belief_price`
/// stays `None` (meaningless across heterogeneous multi-hop pairs);
/// `max_spread` is pinned to the pools' hard cap — see
/// [`per_hop_max_spread`]. End-to-end slippage is enforced via
/// `minimum_receive` in `execute_assert_received`.
fn build_pool_swap_msg(
    operation: &SwapOperation,
    offer_amount: Uint128,
    to: String,
) -> Result<CosmosMsg, RouterError> {
    // Both the bluechip side and the creator TokenFactory side are native
    // bank denoms now, so every hop is a `SimpleSwap` with the offer denom
    // attached as funds. (Pre-migration the `CreatorToken` arm routed
    // through a CW20 `Send` + `PoolSwapCw20HookMsg::Swap`.)
    match &operation.offer_asset_info {
        TokenType::Native { denom } | TokenType::CreatorToken { denom } => {
            let exec = PoolSwapExecuteMsg::SimpleSwap {
                offer_asset: TokenInfo {
                    info: operation.offer_asset_info.clone(),
                    amount: offer_amount,
                },
                belief_price: None,
                max_spread: Some(per_hop_max_spread()),
                to: Some(to),
                transaction_deadline: None,
            };
            Ok(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: operation.pool_addr.clone(),
                msg: to_json_binary(&exec)?,
                funds: vec![Coin {
                    denom: denom.clone(),
                    amount: offer_amount,
                }],
            }))
        }
    }
}

/// Extracts the offer amount from `info.funds` for a native first hop.
/// Requires exactly one coin and that its denom matches the declared
/// first-hop offer denom.
fn extract_native_offer(info: &MessageInfo, denom: &str) -> Result<Uint128, RouterError> {
    if info.funds.len() != 1 {
        return Err(RouterError::Std(StdError::generic_err(
            "ExecuteMultiHop requires exactly one funds coin matching the first hop offer denom",
        )));
    }
    let coin = &info.funds[0];
    if coin.denom != denom {
        return Err(RouterError::Std(StdError::generic_err(format!(
            "funds denom {} does not match first hop offer denom {}",
            coin.denom, denom
        ))));
    }
    Ok(coin.amount)
}

pub fn hop_reply_id(hop_index: u32) -> u64 {
    REPLY_ID_HOP_BASE + hop_index as u64
}

pub fn parse_hop_reply_id(id: u64) -> Option<u32> {
    if id >= REPLY_ID_HOP_BASE && id < REPLY_ID_HOP_BASE + MAX_HOPS as u64 {
        Some((id - REPLY_ID_HOP_BASE) as u32)
    } else {
        None
    }
}
