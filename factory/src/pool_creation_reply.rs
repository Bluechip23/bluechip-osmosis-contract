use crate::{
    error::ContractError,
    execute::{encode_reply_id, FINALIZE_POOL},
    msg::CreatePoolReplyMsg,
    pool_create_cleanup::{extract_contract_address, give_pool_ownership_nft},
    pool_struct::{CommitFeeInfo, PoolDetails, TempPoolCreation},
    state::FACTORYINSTANTIATEINFO,
};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, CosmosMsg, DepsMut, Env, Reply, Response, StdResult, SubMsg,
    WasmMsg,
};

/// CW721 NFT branding for liquidity-position NFTs minted on commit-pool
/// creation. Kept at module scope so a deployment-specific re-skin
/// (white-label, fork) is a single edit per constant. `pub(crate)` so the
/// create handler (which now emits the NFT instantiate as the first step
/// of the reply chain) uses the same branding.
pub(crate) const LP_NFT_NAME: &str = "AMM LP Positions";
pub(crate) const LP_NFT_SYMBOL: &str = "AMM-LP";
pub(crate) const LP_NFT_LABEL_PREFIX: &str = "AMM-LP-NFT-";

// pool_creation_reply.rs
//
// The creator token is a native TokenFactory denom the pool owns, so the
// factory no longer instantiates a CW20. The reply chain is now:
//   create (instantiate NFT) -> mint_create_pool (instantiate pool)
//     -> finalize_pool (register + ownership handoff).
//
// Every step uses `SubMsg::reply_on_success`. Under that dispatch mode, a
// failing submessage bypasses the reply handler and propagates the error
// up through the entire tx, rolling back ALL state writes atomically
// (including prior successful reply handlers' writes and the CW721 / pool
// instantiations themselves). So the handlers below only need to
// implement the happy path; a defensive `into_result` guards against a
// future change to `reply_always` / `reply_on_error` without also updating
// these handlers.
//
// The creation context (`TempPoolCreation`) travels through the chain as
// the SubMsg `payload`, echoed back verbatim in each `Reply`. Because the
// chain is atomic, the context never needs to survive the tx, so carrying
// it in payloads avoids a storage save/load round-trip at every step.
// Each handler deserializes the incoming payload, adds the address it
// just learned, and attaches the updated payload to the next SubMsg.

/// Deserialize the `TempPoolCreation` context from a reply's payload,
/// wrapping decode failures with the step name so a malformed payload
/// (which should be impossible — the factory itself authored it one
/// submessage earlier) is attributable.
fn creation_context_from_payload(
    msg: &Reply,
    step: &'static str,
) -> Result<TempPoolCreation, ContractError> {
    from_json(&msg.payload).map_err(|e| {
        ContractError::Std(cosmwasm_std::StdError::generic_err(format!(
            "{}: invalid pool-creation payload: {}",
            step, e
        )))
    })
}

/// NFT-created reply: the position NFT was instantiated by the create
/// handler; here we learn its address and instantiate the pool wasm. The
/// pool creates its own TokenFactory creator denom from `ctx.subdenom`, so
/// no CW20 address is threaded through — the `CreatePoolReplyMsg` carries
/// the `subdenom` and a placeholder `CreatorToken` slot the pool
/// overwrites. (Formerly this step also consumed a CW20 instantiate reply;
/// that step has been removed.)
pub fn mint_create_pool(
    deps: DepsMut,
    env: Env,
    msg: Reply,
    pool_id: u64,
) -> Result<Response, ContractError> {
    let reply_id = msg.id;
    let result = msg
        .result
        .clone()
        .into_result()
        .map_err(|e| ContractError::ReplyOnSuccessSawError {
            id: reply_id,
            msg: format!("mint_create_pool: {}", e),
        })?;

    let mut ctx = creation_context_from_payload(&msg, "mint_create_pool")?;
    let nft_address = extract_contract_address(&deps, &result)?;
    ctx.nft_addr = Some(nft_address.clone());

    let factory_config = FACTORYINSTANTIATEINFO.load(deps.storage)?;

    // Threshold-payout splits live on `FactoryInstantiate` so they
    // ride the standard 48h propose/apply flow. `validate()` is also
    // called at propose time; calling it here is belt-and-suspenders
    // for old serialized records that bypassed the gate.
    let threshold_payout = factory_config.threshold_payout_amounts.clone();
    threshold_payout.validate()?;

    let threshold_binary = to_json_binary(&threshold_payout)?;

    // Pass the user-supplied pair straight through. Index 1 is the
    // `CreatorToken` placeholder — the pool ignores its denom and builds
    // `factory/{pool_addr}/{ctx.subdenom}` itself at instantiate.
    let commit_msg = CreatePoolReplyMsg {
        pool_id,
        pool_token_info: ctx.temp_pool_info.pool_token_info.clone(),
        used_factory_addr: env.contract.address.clone(),
        threshold_payout: Some(threshold_binary),
        commit_fee_info: CommitFeeInfo {
            bluechip_wallet_address: factory_config.bluechip_wallet_address.clone(),
            creator_wallet_address: ctx.temp_creator_wallet.clone(),
            commit_fee_bluechip: factory_config.commit_fee_bluechip,
            commit_fee_creator: factory_config.commit_fee_creator,
        },
        commit_threshold_limit_usd: factory_config.commit_threshold_limit_usd,
        subdenom: ctx.subdenom.clone(),
        position_nft_address: nft_address.clone(),
        max_bluechip_lock_per_pool: factory_config.max_bluechip_lock_per_pool,
        creator_excess_liquidity_lock_days: factory_config.creator_excess_liquidity_lock_days,
    };
    let pool_msg = WasmMsg::Instantiate {
        code_id: factory_config.create_pool_wasm_contract_id,
        msg: to_json_binary(&commit_msg)?,
        funds: vec![],
        admin: Some(env.contract.address.to_string()),
        label: format!("Pool-{}", pool_id),
    };

    let sub_msg = SubMsg::reply_on_success(pool_msg, encode_reply_id(pool_id, FINALIZE_POOL))
        .with_payload(to_json_binary(&ctx)?);

    Ok(Response::new()
        .add_attribute("action", "nft_created_successfully")
        .add_attribute("nft_address", nft_address)
        .add_attribute("pool_id", pool_id.to_string())
        .add_submessage(sub_msg))
}

pub fn finalize_pool(
    deps: DepsMut,
    _env: Env,
    msg: Reply,
    pool_id: u64,
) -> Result<Response, ContractError> {
    let reply_id = msg.id;
    let result = msg
        .result
        .clone()
        .into_result()
        .map_err(|e| ContractError::ReplyOnSuccessSawError {
            id: reply_id,
            msg: format!("finalize_pool: {}", e),
        })?;

    let ctx = creation_context_from_payload(&msg, "finalize_pool")?;
    let pool_address = extract_contract_address(&deps, &result)?;

    let nft_address = ctx
        .nft_addr
        .clone()
        .ok_or(ContractError::ReplyMissingAddress {
            step: "finalize_pool",
            kind: "nft",
        })?;

    // Reconstruct `pool_token_info` with the creator token's REAL native
    // denom. The creator denom is deterministic — the pool created
    // `factory/{pool_address}/{ctx.subdenom}` at instantiate and is its
    // admin — so we rebuild it here from the now-known pool address rather
    // than reading it back from the pool. The user-supplied
    // `pool_token_info[1]` only carried a placeholder denom; persisting
    // that into `POOLS_BY_ID` would leave every registry entry pointing at
    // the wrong denom. The validator (`validate_pool_token_info`) enforces
    // a strict [Native(bluechip), CreatorToken(placeholder)] shape, so the
    // bluechip side is always at index 0.
    let bluechip_side = ctx.temp_pool_info.pool_token_info[0].clone();
    let creator_denom = pool_core_full_denom(&pool_address, &ctx.subdenom);
    let pool_token_info = [
        bluechip_side,
        crate::asset::TokenType::CreatorToken {
            denom: creator_denom,
        },
    ];

    let pool_details = PoolDetails {
        pool_id,
        pool_token_info,
        creator_pool_addr: pool_address.clone(),
        // This reply handler completes the pool creation chain
        // triggered by ExecuteMsg::Create.
    };

    // NFT-only ownership handoff. The pool is the TokenFactory denom admin
    // from creation, so there is no CW20 minter handoff to perform — only
    // the CW721 position-NFT `TransferOwnership`.
    let ownership_msgs = give_pool_ownership_nft(&nft_address, &pool_address)?;

    // Symmetric two-phase NFT accept.
    // `give_pool_ownership_nft` only emits the CW721 `TransferOwnership`
    // (cw_ownable is two-phase: sets pending_owner, current owner
    // unchanged). Without this trigger, the factory remained the NFT
    // contract's actual owner until the lazy `AcceptOwnership` in
    // `trigger_threshold_payout` fired at threshold cross — potentially
    // never, for a pool that fails to threshold. Dispatching
    // `AcceptNftOwnership {}` to the freshly-created pool here closes that
    // window inside the create tx: the pool's handler emits the matching
    // `AcceptOwnership` to the NFT and the create tx ends with the pool as
    // actual owner.
    let pool_accept_trigger = build_pool_accept_nft_ownership_call(&pool_address)?;

    // Single atomic write across the three pool-registry maps so
    // they cannot drift. See state::register_pool.
    crate::state::register_pool(deps.storage, pool_id, &pool_address, &pool_details)?;

    Ok(Response::new()
        // Order matters: `ownership_msgs` carries the CW721
        // `TransferOwnership` that sets `pending_owner = pool` on the
        // NFT. The accept-trigger executes next, and its handler emits
        // the matching `AcceptOwnership` to the NFT. Reversing the
        // order would have the pool attempt to accept before being
        // staged as `pending_owner` and the NFT contract would reject
        // with `NoPendingOwner`, reverting the entire create tx.
        .add_messages(ownership_msgs)
        .add_message(pool_accept_trigger)
        .add_attribute("action", "pool_created_successfully")
        .add_attribute("pool_address", pool_address)
        .add_attribute("pool_id", pool_id.to_string()))
}

/// Deterministic TokenFactory denom `factory/{admin}/{subdenom}`.
/// Duplicated from `pool_core::osmosis_msgs::full_denom` (the factory has
/// no compile-time dependency on `pool-core`) so `finalize_pool` can
/// reconstruct the pool-owned creator denom from the pool address.
fn pool_core_full_denom(admin: &Addr, subdenom: &str) -> String {
    format!("factory/{}/{}", admin, subdenom)
}

/// Minimal typed mirror of the pool-side ExecuteMsg variants the factory
/// ever needs to call back into. Intentionally NOT a re-export of
/// `creator_pool::msg::ExecuteMsg` —
/// the factory must not take a circular dep on the pool crate. Wire
/// compatibility is locked in by the round-trip
/// parse tests in the pool crate's testing module.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum PoolFactoryCallback {
    AcceptNftOwnership {},
}

/// Builds the `Wasm::Execute { AcceptNftOwnership {} }` call back into
/// a freshly-created pool. Sender on the resulting
/// transaction is the factory contract, which is what the pool-side
/// `execute_accept_nft_ownership` handlers authorise on.
fn build_pool_accept_nft_ownership_call(pool_addr: &cosmwasm_std::Addr) -> StdResult<CosmosMsg> {
    Ok(WasmMsg::Execute {
        contract_addr: pool_addr.to_string(),
        msg: to_json_binary(&PoolFactoryCallback::AcceptNftOwnership {})?,
        funds: vec![],
    }
    .into())
}
