//! Commit-phase-only claim handlers.
//!
//! The single claim here is the time-locked creator-excess release
//! (third-party LP lives on the native pool, not in this contract).
//!
//! The release transfers the RAW earmarked coins — `bluechip_amount`
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
    execute_claim_creator_excess_inner(deps, env, info)
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
    // The earmarked creator tokens only exist once the pool has been
    // seeded (they are part of the pool-seed mint). Before that the claim
    // waits — unless the pool was drained first, in which case the seed
    // mint will never happen and only the OSMO earmark is payable.
    let mut excess_position = excess_position;
    if crate::state::PENDING_POOL_SEED
        .may_load(deps.storage)?
        .is_some()
    {
        let drained = crate::state::EMERGENCY_DRAINED
            .may_load(deps.storage)?
            .unwrap_or(false);
        if !drained {
            return Err(ContractError::DistributionInProgress {});
        }
        excess_position.token_amount = cosmwasm_std::Uint128::zero();
    }

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
        (
            "bluechip_amount",
            excess_position.bluechip_amount.to_string(),
        ),
        ("creator_denom", creator_denom),
        ("token_amount", excess_position.token_amount.to_string()),
        ("pool_contract", env.contract.address.to_string()),
        ("block_height", env.block.height.to_string()),
        ("block_time", env.block.time.seconds().to_string()),
    ]))
}
