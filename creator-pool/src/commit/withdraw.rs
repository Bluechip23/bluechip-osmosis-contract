//! Committer withdrawal from a pool that never reached its threshold.
//!
//! A commit is normally final. The one exit is this handler: once a pool
//! has existed for `COMMIT_WITHDRAW_LOCK_SECONDS` (18 months) WITHOUT
//! crossing its threshold, each committer may withdraw their own
//! contribution. The withdrawal:
//!
//! - refunds the committer's NET contribution (`COMMIT_NET_LEDGER`) — the
//!   OSMO the pool actually holds for them. The 1% protocol and 5% creator
//!   fees were paid out when each commit landed and are not refunded;
//! - subtracts their GROSS contribution from the amount committed toward
//!   the threshold (`GROSS_NATIVE_COMMITTED`) and their net from
//!   `NATIVE_RAISED_FROM_COMMIT`, so the pool's totals always equal the sum
//!   of the remaining ledger rows;
//! - removes them from the ledgers, so it is one-shot and they earn no
//!   airdrop if the pool crosses later. They may commit again afterwards.
//!
//! The pool stays open: others can keep committing and it can still cross.
//! Once it has crossed, withdrawal is closed for good.
//!
//! Deliberately NOT gated on the admin pause, and it makes no factory
//! query: this is the committer's guaranteed exit, so neither a paused
//! pool nor a broken fee route can block it.

use cosmwasm_std::{BankMsg, Coin, DepsMut, Env, MessageInfo, Response};

use crate::asset::get_native_denom;
use crate::error::ContractError;
use crate::state::{
    COMMITTER_COUNT, COMMIT_INFO, COMMIT_LEDGER, COMMIT_NET_LEDGER, COMMIT_WITHDRAW_LOCK_SECONDS,
    GROSS_NATIVE_COMMITTED, IS_THRESHOLD_HIT, NATIVE_RAISED_FROM_COMMIT, POOL_CREATED_AT,
    POOL_INFO,
};

pub fn execute_withdraw_commit(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    if IS_THRESHOLD_HIT.may_load(deps.storage)?.unwrap_or(false) {
        return Err(ContractError::CommitWithdrawAfterThreshold {});
    }

    let unlock_time = POOL_CREATED_AT
        .load(deps.storage)?
        .plus_seconds(COMMIT_WITHDRAW_LOCK_SECONDS);
    if env.block.time < unlock_time {
        return Err(ContractError::CommitWithdrawLocked { unlock_time });
    }

    let gross = COMMIT_LEDGER
        .may_load(deps.storage, &info.sender)?
        .filter(|g| !g.is_zero())
        .ok_or_else(|| ContractError::NoCommitToWithdraw {
            user: info.sender.to_string(),
        })?;
    let net = COMMIT_NET_LEDGER
        .may_load(deps.storage, &info.sender)?
        .unwrap_or_default();

    // Remove the committer everywhere before paying out (one-shot).
    COMMIT_LEDGER.remove(deps.storage, &info.sender);
    COMMIT_NET_LEDGER.remove(deps.storage, &info.sender);
    COMMIT_INFO.remove(deps.storage, &info.sender);
    let count = COMMITTER_COUNT.may_load(deps.storage)?.unwrap_or(0);
    COMMITTER_COUNT.save(deps.storage, &count.saturating_sub(1))?;

    // Subtract from the pool totals. Checked: a ledger row can never exceed
    // the total it was added into, so an underflow here means corrupted
    // state and the withdrawal must not proceed.
    let total_gross = GROSS_NATIVE_COMMITTED
        .load(deps.storage)?
        .checked_sub(gross)?;
    GROSS_NATIVE_COMMITTED.save(deps.storage, &total_gross)?;
    let total_net = NATIVE_RAISED_FROM_COMMIT
        .load(deps.storage)?
        .checked_sub(net)?;
    NATIVE_RAISED_FROM_COMMIT.save(deps.storage, &total_net)?;

    let pool_info = POOL_INFO.load(deps.storage)?;
    let mut response = Response::new();
    if !net.is_zero() {
        response = response.add_message(BankMsg::Send {
            to_address: info.sender.to_string(),
            amount: vec![Coin {
                denom: get_native_denom(&pool_info.pool_info.asset_infos)?,
                amount: net,
            }],
        });
    }

    Ok(response
        .add_attribute("action", "withdraw_commit")
        .add_attribute("committer", info.sender.to_string())
        .add_attribute("gross_removed", gross.to_string())
        .add_attribute("refunded", net.to_string())
        .add_attribute("total_committed_after", total_gross.to_string())
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}
