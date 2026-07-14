//! Shared admin handlers: pause/unpause, cancel-emergency-withdraw,
//! factory config updates, and the two-phase emergency withdraw split.
//!
//! Phase-2 note: the internal AMM (reserves + LP positions + fee reserves)
//! is gone. The per-position emergency-claim escrow it backed
//! (`ClaimEmergencyShare` / `SweepUnclaimedEmergencyShares` /
//! `EmergencyDrainSnapshot`) was removed with it. Emergency withdraw is now
//! a simple two-phase pause+timelock followed by a drain that sweeps the
//! pool's held `gamm/pool/{id}` LP shares (and any residual bluechip /
//! creator-token bank balance) to the bluechip wallet.

use crate::asset::query_balance;
use crate::error::ContractError;
use crate::msg::PoolConfigUpdate;
use crate::state::{
    EmergencyWithdrawalInfo, COMMITFEEINFO, EMERGENCY_DRAINED, EMERGENCY_WITHDRAWAL,
    PENDING_EMERGENCY_WITHDRAW, POOL_ID, POOL_INFO, POOL_PAUSED, POOL_PAUSED_AUTO, POOL_SPECS,
};
use cosmwasm_std::{
    Addr, BankMsg, Coin, CosmosMsg, Decimal, DepsMut, Env, MessageInfo, Response, StdError, Storage,
    Uint128,
};
use pool_factory_interfaces::{EmergencyWithdrawDelayResponse, FactoryQueryMsg};

/// Bundle returned by `execute_emergency_withdraw_core_drain`. Callers
/// turn it into a `Response` after adding any contract-specific
/// bookkeeping.
pub struct CoreDrainResult {
    pub messages: Vec<CosmosMsg>,
    pub total_0: Uint128,
    pub total_1: Uint128,
    pub recipient: Addr,
    pub total_liquidity_at_withdrawal: Uint128,
}

/// Checks that the pool has not been permanently drained.
pub fn ensure_not_drained(storage: &dyn Storage) -> Result<(), ContractError> {
    if EMERGENCY_DRAINED.may_load(storage)?.unwrap_or(false) {
        return Err(ContractError::EmergencyDrained {});
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pause / Unpause
// ---------------------------------------------------------------------------

pub fn execute_pause(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }
    let pool_contract = pool_info.pool_info.contract_addr.to_string();
    POOL_PAUSED.save(deps.storage, &true)?;
    POOL_PAUSED_AUTO.save(deps.storage, &false)?;
    Ok(Response::new()
        .add_attribute("action", "pause")
        .add_attribute("pool_contract", pool_contract)
        .add_attribute("paused_by", info.sender.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

pub fn execute_unpause(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }
    let pool_contract = pool_info.pool_info.contract_addr.to_string();
    POOL_PAUSED.save(deps.storage, &false)?;
    POOL_PAUSED_AUTO.save(deps.storage, &false)?;
    Ok(Response::new()
        .add_attribute("action", "unpause")
        .add_attribute("pool_contract", pool_contract)
        .add_attribute("unpaused_by", info.sender.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

// ---------------------------------------------------------------------------
// Emergency Withdraw — Phase 1: initiate (pause + timelock)
// ---------------------------------------------------------------------------

pub fn execute_emergency_withdraw_initiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }
    ensure_not_drained(deps.storage)?;

    if PENDING_EMERGENCY_WITHDRAW.may_load(deps.storage)?.is_some() {
        return Err(ContractError::Std(StdError::generic_err(
            "Emergency withdraw already initiated; wait for the timelock to elapse or cancel.",
        )));
    }

    let now = env.block.time;
    POOL_PAUSED.save(deps.storage, &true)?;
    POOL_PAUSED_AUTO.save(deps.storage, &false)?;
    let delay: EmergencyWithdrawDelayResponse = deps.querier.query_wasm_smart(
        pool_info.factory_addr.to_string(),
        &pool_factory_interfaces::FactoryQueryEnvelope::PoolFactoryQuery(
            FactoryQueryMsg::EmergencyWithdrawDelaySeconds {},
        ),
    )?;
    let effective_after = now.plus_seconds(delay.delay_seconds);
    PENDING_EMERGENCY_WITHDRAW.save(deps.storage, &effective_after)?;

    Ok(Response::new()
        .add_attribute("action", "emergency_withdraw_initiated")
        .add_attribute("effective_after", effective_after.to_string())
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("initiated_by", info.sender.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

// ---------------------------------------------------------------------------
// Emergency Withdraw — Phase 2: core drain
// ---------------------------------------------------------------------------

/// Drains the pool after the Phase-1 timelock elapses.
///
/// Phase-2 semantics: the pool holds `gamm/pool/{id}` LP shares (its seed
/// liquidity on the native Osmosis pool) plus whatever residual bluechip /
/// creator-token bank balance remains. This drain sweeps the LP shares to
/// the (live-queried) bluechip wallet and flips `EMERGENCY_DRAINED`.
///
/// `accumulation_drain_*` are additional coin amounts the caller's
/// contract-specific bookkeeping wants folded into the drain (creator-pool
/// forwards zero now that the creator-excess entitlement is LP-share-based
/// and released on its own timelock). They are recorded in the drain
/// totals but not otherwise transferred here.
pub fn execute_emergency_withdraw_core_drain(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    _accumulation_drain_0: Uint128,
    _accumulation_drain_1: Uint128,
) -> Result<CoreDrainResult, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }
    ensure_not_drained(deps.storage)?;

    let effective_after = PENDING_EMERGENCY_WITHDRAW
        .may_load(deps.storage)?
        .ok_or_else(|| {
            ContractError::Std(StdError::generic_err(
                "Emergency withdraw has not been initiated.",
            ))
        })?;

    if env.block.time < effective_after {
        return Err(ContractError::EmergencyTimelockPending { effective_after });
    }

    PENDING_EMERGENCY_WITHDRAW.remove(deps.storage);

    // Live-query the bluechip wallet (falls back to the instantiate-time
    // snapshot if the factory is unreachable, so the drain can't be
    // stranded by a factory outage).
    let fee_info = COMMITFEEINFO.load(deps.storage)?;
    let recipient = match deps
        .querier
        .query_wasm_smart::<pool_factory_interfaces::BluechipWalletResponse>(
            pool_info.factory_addr.to_string(),
            &pool_factory_interfaces::FactoryQueryEnvelope::PoolFactoryQuery(
                pool_factory_interfaces::FactoryQueryMsg::BluechipWalletAddress {},
            ),
        ) {
        Ok(resp) => resp.address,
        Err(_) => fee_info.bluechip_wallet_address.clone(),
    };

    // Sweep the pool's held native LP shares (`gamm/pool/{id}`), if any.
    let mut messages: Vec<CosmosMsg> = vec![];
    let mut lp_shares = Uint128::zero();
    if let Some(pool_id) = POOL_ID.may_load(deps.storage)? {
        let lp_denom = format!("gamm/pool/{}", pool_id);
        lp_shares = query_balance(
            &deps.querier,
            env.contract.address.clone(),
            lp_denom.clone(),
        )
        .unwrap_or_default();
        if !lp_shares.is_zero() {
            messages.push(CosmosMsg::Bank(BankMsg::Send {
                to_address: recipient.to_string(),
                amount: vec![Coin {
                    denom: lp_denom,
                    amount: lp_shares,
                }],
            }));
        }
    }

    let withdrawal_info = EmergencyWithdrawalInfo {
        withdrawn_at: env.block.time.seconds(),
        recipient: recipient.clone(),
        amount0: lp_shares,
        amount1: Uint128::zero(),
        total_liquidity_at_withdrawal: lp_shares,
    };
    EMERGENCY_WITHDRAWAL.save(deps.storage, &withdrawal_info)?;

    EMERGENCY_DRAINED.save(deps.storage, &true)?;

    Ok(CoreDrainResult {
        messages,
        total_0: lp_shares,
        total_1: Uint128::zero(),
        recipient,
        total_liquidity_at_withdrawal: lp_shares,
    })
}

// ---------------------------------------------------------------------------
// Emergency Withdraw — cancel (pre-drain only)
// ---------------------------------------------------------------------------

pub fn execute_cancel_emergency_withdraw(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }
    if PENDING_EMERGENCY_WITHDRAW.may_load(deps.storage)?.is_none() {
        return Err(ContractError::NoPendingEmergencyWithdraw {});
    }
    PENDING_EMERGENCY_WITHDRAW.remove(deps.storage);
    POOL_PAUSED.save(deps.storage, &false)?;
    POOL_PAUSED_AUTO.save(deps.storage, &false)?;
    Ok(Response::new()
        .add_attribute("action", "emergency_withdraw_cancelled")
        .add_attribute(
            "pool_contract",
            pool_info.pool_info.contract_addr.to_string(),
        )
        .add_attribute("cancelled_by", info.sender.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

// ---------------------------------------------------------------------------
// Config update (factory-only)
// ---------------------------------------------------------------------------

pub fn execute_update_config_from_factory(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    update: PoolConfigUpdate,
) -> Result<Response, ContractError> {
    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }

    let mut attributes = vec![("action", "update_config")];
    let mut specs = POOL_SPECS.load(deps.storage)?;
    let mut specs_changed = false;

    if let Some(fee) = update.lp_fee {
        let max_lp_fee = Decimal::percent(10);
        let min_lp_fee = Decimal::permille(1); // 0.1%
        if fee > max_lp_fee {
            return Err(ContractError::Std(StdError::generic_err(
                "lp_fee must not exceed 10% (0.1)",
            )));
        }
        if fee < min_lp_fee {
            return Err(ContractError::Std(StdError::generic_err(
                "lp_fee must be at least 0.1% (0.001)",
            )));
        }
        specs.lp_fee = fee;
        specs_changed = true;
        attributes.push(("lp_fee", "updated"));
    }

    if let Some(interval) = update.min_commit_interval {
        const MAX_COMMIT_INTERVAL: u64 = 86_400; // 24 hours
        if interval > MAX_COMMIT_INTERVAL {
            return Err(ContractError::Std(StdError::generic_err(
                "min_commit_interval must not exceed 86400 seconds (1 day)",
            )));
        }
        specs.min_commit_interval = interval;
        specs_changed = true;
        attributes.push(("min_commit_interval", "updated"));
    }

    if specs_changed {
        POOL_SPECS.save(deps.storage, &specs)?;
    }

    Ok(Response::new()
        .add_attributes(attributes)
        .add_attribute(
            "pool_contract",
            pool_info.pool_info.contract_addr.to_string(),
        )
        .add_attribute("updated_by", info.sender.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}

/// Two-phase emergency-withdraw dispatcher for consuming contracts.
pub fn execute_emergency_withdraw_dispatch(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    accumulation_drain_0: Uint128,
    accumulation_drain_1: Uint128,
) -> Result<Response, ContractError> {
    if PENDING_EMERGENCY_WITHDRAW.may_load(deps.storage)?.is_none() {
        return execute_emergency_withdraw_initiate(deps, env, info);
    }
    let drain = execute_emergency_withdraw_core_drain(
        deps,
        env.clone(),
        info,
        accumulation_drain_0,
        accumulation_drain_1,
    )?;
    Ok(Response::new()
        .add_messages(drain.messages)
        .add_attribute("action", "emergency_withdraw")
        .add_attribute("recipient", drain.recipient)
        .add_attribute("amount0", drain.total_0)
        .add_attribute("amount1", drain.total_1)
        .add_attribute("total_liquidity", drain.total_liquidity_at_withdrawal)
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("block_height", env.block.height.to_string())
        .add_attribute("block_time", env.block.time.seconds().to_string()))
}
