//! Commit-phase-only claim handlers.
//!
//! Phase-2: the internal LP system and the creator fee-pot are gone. The
//! only claim that survives is the time-locked creator-excess release.
//!
//! FIX C: the release transfers the RAW earmarked coins — `bluechip_amount`
//! (bluechip denom) + `token_amount` (creator denom) — that were parked in
//! the contract's bank balance at threshold crossing, straight to the
//! creator. There is no LP-share query/proportion anymore.

use crate::asset::get_native_denom;
use crate::error::ContractError;
use crate::state::{CreatorExcessLiquidity, CREATOR_EXCESS_POSITION, POOL_INFO};
use cosmwasm_std::{BankMsg, Coin, CosmosMsg, DepsMut, Env, MessageInfo, Response, Timestamp};

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

    // Creator-only. Unchanged auth model.
    if info.sender != excess_position.creator {
        return Err(ContractError::Unauthorized {});
    }

    // Unlock-time gate. Unchanged.
    if env.block.time < excess_position.unlock_time {
        return Err(ContractError::PositionLocked {
            unlock_time: excess_position.unlock_time,
        });
    }

    // Resolve the two denoms: bluechip is the Native side, the creator
    // token is the pool's TokenFactory denom.
    let bluechip_denom = get_native_denom(&pool_info.pool_info.asset_infos)?;
    let creator_denom = pool_info.token_denom.clone();

    // One-shot: remove the entitlement AFTER capturing its amounts (the
    // BankMsg::Send messages below carry the captured values).
    CREATOR_EXCESS_POSITION.remove(deps.storage);

    // Send the RAW earmarked coins to the creator. The contract holds both
    // (excess bluechip from commits + excess creator tokens minted-but-not-
    // seeded at crossing), so this spends the contract's own bank balance.
    let mut coins: Vec<Coin> = vec![];
    if !excess_position.bluechip_amount.is_zero() {
        coins.push(Coin {
            denom: bluechip_denom.clone(),
            amount: excess_position.bluechip_amount,
        });
    }
    if !excess_position.token_amount.is_zero() {
        coins.push(Coin {
            denom: creator_denom.clone(),
            amount: excess_position.token_amount,
        });
    }

    let mut messages: Vec<CosmosMsg> = vec![];
    if !coins.is_empty() {
        messages.push(CosmosMsg::Bank(BankMsg::Send {
            to_address: excess_position.creator.to_string(),
            amount: coins,
        }));
    }

    Ok(Response::new().add_messages(messages).add_attributes(vec![
        ("action", "claim_creator_excess".to_string()),
        ("creator", excess_position.creator.to_string()),
        ("bluechip_denom", bluechip_denom),
        ("bluechip_amount", excess_position.bluechip_amount.to_string()),
        ("creator_denom", creator_denom),
        ("token_amount", excess_position.token_amount.to_string()),
        ("pool_contract", env.contract.address.to_string()),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
    ]))
}
