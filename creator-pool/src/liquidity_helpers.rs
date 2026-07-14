//! Commit-phase-only claim handlers.
//!
//! Phase-2: the internal LP system and the creator fee-pot are gone. The
//! only claim that survives is the time-locked creator-excess release,
//! which now transfers a proportional slice of the pool-held
//! `gamm/pool/{id}` seed LP shares to the creator.

use crate::error::ContractError;
use crate::state::{CreatorExcessLiquidity, CREATOR_EXCESS_POSITION, POOL_ID, POOL_INFO};
use cosmwasm_std::{BankMsg, Coin, CosmosMsg, DepsMut, Env, MessageInfo, Response, Timestamp, Uint128};
use pool_core::asset::query_balance;

pub fn execute_claim_creator_excess(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    transaction_deadline: Option<Timestamp>,
) -> Result<Response, ContractError> {
    crate::generic_helpers::enforce_transaction_deadline(env.block.time, transaction_deadline)?;
    crate::generic_helpers::with_reentrancy_guard(deps, |deps| {
        execute_claim_creator_excess_inner(deps, env, info)
    })
}

fn execute_claim_creator_excess_inner(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let excess_position: CreatorExcessLiquidity = CREATOR_EXCESS_POSITION.load(deps.storage)?;
    let pool_info = POOL_INFO.load(deps.storage)?;

    if info.sender != excess_position.creator {
        return Err(ContractError::Unauthorized {});
    }

    if env.block.time < excess_position.unlock_time {
        return Err(ContractError::PositionLocked {
            unlock_time: excess_position.unlock_time,
        });
    }

    // Compute the creator's LP-share slice from the pool's CURRENT
    // `gamm/pool/{id}` balance: the pool holds all of its seed LP shares
    // permanently, so its balance IS the total seed LP. The slice is
    // `total_seed_lp * excess_bluechip / total_seeded_bluechip`.
    let pool_id = POOL_ID
        .may_load(deps.storage)?
        .ok_or(ContractError::ShortOfThreshold {})?;
    let lp_denom = format!("gamm/pool/{}", pool_id);
    let total_seed_lp = query_balance(
        &deps.querier,
        pool_info.pool_info.contract_addr.clone(),
        lp_denom.clone(),
    )?;

    let lp_share = if excess_position.total_seeded_bluechip.is_zero() {
        Uint128::zero()
    } else {
        total_seed_lp.multiply_ratio(
            excess_position.excess_bluechip,
            excess_position.total_seeded_bluechip,
        )
    };

    // One-shot: remove the entitlement AFTER building the message.
    CREATOR_EXCESS_POSITION.remove(deps.storage);

    let mut messages: Vec<CosmosMsg> = vec![];
    if !lp_share.is_zero() {
        messages.push(CosmosMsg::Bank(BankMsg::Send {
            to_address: excess_position.creator.to_string(),
            amount: vec![Coin {
                denom: lp_denom.clone(),
                amount: lp_share,
            }],
        }));
    }

    Ok(Response::new().add_messages(messages).add_attributes(vec![
        ("action", "claim_creator_excess".to_string()),
        ("creator", excess_position.creator.to_string()),
        ("lp_denom", lp_denom),
        ("lp_shares", lp_share.to_string()),
        ("excess_bluechip", excess_position.excess_bluechip.to_string()),
        (
            "total_seeded_bluechip",
            excess_position.total_seeded_bluechip.to_string(),
        ),
        ("pool_contract", env.contract.address.to_string()),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
    ]))
}
