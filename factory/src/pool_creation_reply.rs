use crate::{
    error::ContractError,
    pool_create_cleanup::extract_contract_address,
    pool_struct::PoolDetails,
    pool_struct::TempPoolCreation,
};
use cosmwasm_std::{from_json, Addr, DepsMut, Env, Reply, Response};

// pool_creation_reply.rs
//
// Phase-2: the pool no longer takes a position NFT (the internal LP system
// was removed), and the creator token is a native TokenFactory denom the
// pool owns. The reply chain therefore collapses to a single step:
//   create (instantiate pool) -> finalize_pool (register).
//
// The step uses `SubMsg::reply_on_success`; a failing submessage bypasses
// the reply handler and propagates the error up through the whole tx,
// rolling back ALL state writes atomically. The creation context
// (`TempPoolCreation`) rides the SubMsg payload, echoed back in the
// `Reply`, so it never needs a storage round-trip.

/// Deserialize the `TempPoolCreation` context from a reply's payload.
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

    // Reconstruct `pool_token_info` with the creator token's REAL native
    // denom. The creator denom is deterministic — the pool created
    // `factory/{pool_address}/{ctx.subdenom}` at instantiate and is its
    // admin — so rebuild it here from the now-known pool address.
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
    };

    // Single atomic write across the three pool-registry maps.
    crate::state::register_pool(deps.storage, pool_id, &pool_address, &pool_details)?;

    Ok(Response::new()
        .add_attribute("action", "pool_created_successfully")
        .add_attribute("pool_address", pool_address)
        .add_attribute("pool_id", pool_id.to_string()))
}

/// Deterministic TokenFactory denom `factory/{admin}/{subdenom}`.
/// Duplicated from `pool_core::osmosis_msgs::full_denom` (the factory has
/// no compile-time dependency on `pool-core`).
fn pool_core_full_denom(admin: &Addr, subdenom: &str) -> String {
    format!("factory/{}/{}", admin, subdenom)
}
