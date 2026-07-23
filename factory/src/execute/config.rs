//! Factory- and pool-level config propose/apply/cancel handlers.
//!
//! Every handler in this module is admin-only (gated through
//! [`super::ensure_admin`]) and, for the propose/apply pairs, subject to
//! the standard 48h [`ADMIN_TIMELOCK_SECONDS`] timelock so the community
//! has a full two-day observability window before a mutation lands.

use cosmwasm_std::{
    to_json_binary, CosmosMsg, DepsMut, Env, MessageInfo, Response, StdError, WasmMsg,
};

use crate::error::ContractError;
use crate::pool_struct::PoolConfigUpdate;
use crate::state::{
    FactoryInstantiate, PendingConfig, PendingPoolConfig, ADMIN_TIMELOCK_SECONDS,
    FACTORYINSTANTIATEINFO, PENDING_CONFIG, PENDING_POOL_CONFIG, POOLS_BY_ID,
};

use super::ensure_admin;

/// Validates every caller-supplied address + the bluechip_denom on a
/// `FactoryInstantiate` payload, then live-probes the pricing route.
/// Shared between `instantiate` and
/// `execute_propose_factory_config_update` so the same rules apply to
/// the initial config and any subsequent config proposal.
pub(crate) fn validate_factory_config(
    deps: cosmwasm_std::Deps,
    env: &Env,
    config: &FactoryInstantiate,
) -> Result<(), ContractError> {
    deps.api
        .addr_validate(config.factory_admin_address.as_str())?;
    deps.api
        .addr_validate(config.bluechip_wallet_address.as_str())?;

    // Commit fees split bluechip + creator out of every commit. Their sum
    // must not exceed 100% — anything more would either underflow at
    // payout time or cause the pool's instantiate to reject (`InvalidFee`),
    // bricking new pool creation until another full 48h timelock cycle to
    // fix. Pool's instantiate enforces the same invariant; checking here
    // as well surfaces the misconfig at propose time.
    let fee_sum = config
        .commit_fee_bluechip
        .checked_add(config.commit_fee_creator)
        .map_err(|_| ContractError::Std(StdError::generic_err("commit fee sum overflow")))?;
    if fee_sum > cosmwasm_std::Decimal::one() {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "commit_fee_bluechip + commit_fee_creator must be <= 1.0; got {}",
            fee_sum
        ))));
    }

    // A zero threshold would make the pool's commit threshold
    // uncrossable — every commit-pool created against this config would
    // permanently sit pre-threshold, never minting, never opening swaps.
    // Reject explicitly rather than letting that misconfig ride through
    // a 48h timelock.
    if config.commit_threshold_limit_usd.is_zero() {
        return Err(ContractError::Std(StdError::generic_err(
            "commit_threshold_limit_usd must be non-zero",
        )));
    }
    if config.bluechip_denom.trim().is_empty() {
        return Err(ContractError::Std(StdError::generic_err(
            "bluechip_denom must be non-empty",
        )));
    }
    // USD pricing config. Every commit is valued through the x/twap of
    // (pricing_pool_id, bluechip_denom / usd_quote_denom), so a broken
    // value here bricks commits chain-wide; validate at propose time.
    if config.pricing_pool_id == 0 {
        return Err(ContractError::Std(StdError::generic_err(
            "pricing_pool_id must be non-zero (the Osmosis pool whose TWAP prices              bluechip_denom in usd_quote_denom)",
        )));
    }
    if config.usd_quote_denom.trim().is_empty() {
        return Err(ContractError::Std(StdError::generic_err(
            "usd_quote_denom must be non-empty (e.g. the USDC denom on this chain)",
        )));
    }
    if config.usd_quote_denom == config.bluechip_denom {
        return Err(ContractError::Std(StdError::generic_err(
            "usd_quote_denom must differ from bluechip_denom",
        )));
    }
    if config.twap_window_seconds < crate::usd_price::TWAP_WINDOW_MIN_SECONDS
        || config.twap_window_seconds > crate::usd_price::TWAP_WINDOW_MAX_SECONDS
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "twap_window_seconds {} outside allowed range [{}, {}]",
            config.twap_window_seconds,
            crate::usd_price::TWAP_WINDOW_MIN_SECONDS,
            crate::usd_price::TWAP_WINDOW_MAX_SECONDS,
        ))));
    }

    // Multi-pool median-oracle shape validation. The primary source is
    // covered by the pricing checks above; validate each EXTRA source and the
    // quorum here so a malformed oracle set fails at propose time rather than
    // bricking every valuation after the 48h timelock.
    let total_sources = 1 + config.oracle.extra_sources.len();
    for (i, s) in config.oracle.extra_sources.iter().enumerate() {
        if s.pool_id == 0 {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "oracle.extra_sources[{}].pool_id must be non-zero",
                i
            ))));
        }
        if s.quote_denom.trim().is_empty() {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "oracle.extra_sources[{}].quote_denom must be non-empty",
                i
            ))));
        }
        if s.quote_denom == config.bluechip_denom {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "oracle.extra_sources[{}].quote_denom must differ from bluechip_denom",
                i
            ))));
        }
        if s.quote_decimals > 30 {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "oracle.extra_sources[{}].quote_decimals {} is implausibly large",
                i, s.quote_decimals
            ))));
        }
        // Routed source: validate the quote->USD leg. A `None` leg means the
        // quote denom is itself the USD stable (direct source).
        if let Some(leg) = &s.usd_leg {
            if leg.pool_id == 0 {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "oracle.extra_sources[{}].usd_leg.pool_id must be non-zero",
                    i
                ))));
            }
            if leg.usd_denom.trim().is_empty() {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "oracle.extra_sources[{}].usd_leg.usd_denom must be non-empty",
                    i
                ))));
            }
            if leg.usd_denom == s.quote_denom {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "oracle.extra_sources[{}].usd_leg.usd_denom must differ from the source's \
                     quote_denom (the leg must actually convert the intermediate to USD)",
                    i
                ))));
            }
            if leg.usd_denom == config.bluechip_denom {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "oracle.extra_sources[{}].usd_leg.usd_denom must differ from bluechip_denom",
                    i
                ))));
            }
            if leg.usd_decimals > 30 {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "oracle.extra_sources[{}].usd_leg.usd_decimals {} is implausibly large",
                    i, leg.usd_decimals
                ))));
            }
        }
    }
    // Reject duplicate pool ids across the whole source set (primary + extras).
    // The median's manipulation resistance rests on ONE independent vote per
    // pool; letting the same pool appear twice would give a manipulated pool
    // multiple correlated votes and skew the median toward it. A multi-asset
    // pool that could price against several denoms must still be listed once.
    let mut seen_pool_ids = std::collections::HashSet::new();
    seen_pool_ids.insert(config.pricing_pool_id);
    for (i, s) in config.oracle.extra_sources.iter().enumerate() {
        if !seen_pool_ids.insert(s.pool_id) {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "oracle.extra_sources[{}].pool_id {} is a duplicate — each pricing pool may \
                 appear only once (incl. the primary pricing_pool_id) so it gets one vote in \
                 the median",
                i, s.pool_id
            ))));
        }
    }
    if config.oracle.min_valid_sources as usize > total_sources {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "oracle.min_valid_sources {} exceeds the {} configured pricing sources \
             (1 primary + {} extra)",
            config.oracle.min_valid_sources,
            total_sources,
            config.oracle.extra_sources.len()
        ))));
    }

    // Live probe of the pricing route. The syntactic checks above
    // cannot tell a typo'd pool id (or a pool missing one of the two
    // denoms, or one too young for the window) from a working route —
    // and because the price path is fail-closed, that typo would
    // otherwise surface only as a chain-wide commit outage costing a
    // further 48h timelock cycle to repair. Running the actual x/twap
    // query against the proposed config turns it into an instant
    // instantiate/propose/apply-time error. The parsed rate also rides
    // through the zero / dust / RATE_MAX sanity gates, so a
    // wrong-decimals quote denom is caught here too.
    crate::usd_price::probe_native_usd_rate(deps, env, config).map_err(|e| {
        ContractError::Std(StdError::generic_err(format!(
            "pricing config failed live TWAP probe (pool {}, {}/{}, window {}s): {}",
            config.pricing_pool_id,
            config.bluechip_denom,
            config.usd_quote_denom,
            config.twap_window_seconds,
            e
        )))
    })?;

    // Threshold-payout splits are stored on FactoryInstantiate so they
    // ride the standard 48h propose/apply flow rather than requiring a
    // contract migration. Validate non-zero components + no overflow at
    // propose time so a misconfig is caught before the timelock starts.
    config.threshold_payout_amounts.validate()?;

    // Range-validate the emergency-withdraw delay. Below the floor, the
    // post-incident response window collapses to nothing meaningful and
    // a compromised admin key could drain reserves before the community
    // observes the timelock. Above the ceiling, even legitimate
    // operational use becomes painful and admins may be tempted to
    // bypass the flow entirely.
    if config.emergency_withdraw_delay_seconds < crate::state::EMERGENCY_WITHDRAW_DELAY_MIN_SECONDS
        || config.emergency_withdraw_delay_seconds
            > crate::state::EMERGENCY_WITHDRAW_DELAY_MAX_SECONDS
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "emergency_withdraw_delay_seconds {} outside allowed range [{}, {}]",
            config.emergency_withdraw_delay_seconds,
            crate::state::EMERGENCY_WITHDRAW_DELAY_MIN_SECONDS,
            crate::state::EMERGENCY_WITHDRAW_DELAY_MAX_SECONDS,
        ))));
    }

    // H-01 — the GAMM pool-creation-fee config. Two payable shapes exist:
    // - denom == bluechip_denom (osmo-test-5: 1 OSMO): the pool retains
    //   this much bluechip from the 1% commit fee and the gamm module
    //   charges it straight from the pool's native balance;
    // - denom == usd_quote_denom (osmosis-1: 20 Noble USDC): the pool
    //   still retains NATIVE from the 1% fee (sized at the live TWAP
    //   rate) and swaps it into the fee coin through the pricing pool at
    //   crossing — the pricing pool trades native/usd_quote by
    //   definition, so the route always exists.
    // Any other denom is unroutable at crossing; reject it up front
    // rather than letting it ride a 48h timelock and brick crossings. A
    // zero amount disables the reserve (the crossing then pays the whole
    // fee out of the seed, still covered by the live-fee query).
    if !config.gamm_pool_creation_fee.amount.is_zero()
        && config.gamm_pool_creation_fee.denom != config.bluechip_denom
        && config.gamm_pool_creation_fee.denom != config.usd_quote_denom
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "gamm_pool_creation_fee.denom must be bluechip_denom \"{}\" or usd_quote_denom \
             \"{}\" (the pricing pool's quote side, swappable at crossing); got \"{}\"",
            config.bluechip_denom, config.usd_quote_denom, config.gamm_pool_creation_fee.denom
        ))));
    }

    Ok(())
}

pub fn execute_update_factory_config(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;

    let pending = PENDING_CONFIG.load(deps.storage)?;

    if env.block.time < pending.effective_after {
        return Err(ContractError::TimelockNotExpired {
            effective_after: pending.effective_after,
        });
    }

    // Re-validate at apply time. Between propose (48h ago) and apply,
    // on-chain state can have moved (the pricing pool could have been
    // drained or pruned); re-running the validation — including the
    // live TWAP probe — catches stale-proposal hazards before the
    // state lands.
    validate_factory_config(deps.as_ref(), &env, &pending.new_config)?;

    FACTORYINSTANTIATEINFO.save(deps.storage, &pending.new_config)?;
    PENDING_CONFIG.remove(deps.storage);

    Ok(Response::new().add_attribute("action", "execute_update_config"))
}

pub fn execute_propose_factory_config_update(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    config: FactoryInstantiate,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;

    // Reject when a config proposal is already pending. Without this, a
    // re-propose silently overwrites the prior pending config and resets
    // the 48h timelock — a benign-looking change observed by the
    // community could be swapped for a hostile one minutes before the
    // window elapses, and watchers polling `PENDING_CONFIG` would just
    // see "still pending" without any explicit cancellation event.
    // Mirrors the pool-config / pool-upgrade propose handlers, which
    // already require an explicit `Cancel` before re-proposing.
    if PENDING_CONFIG.may_load(deps.storage)?.is_some() {
        return Err(ContractError::Std(StdError::generic_err(
            "A factory config update is already pending. Cancel it first via CancelConfigUpdate.",
        )));
    }

    // Validate at propose time so any mistake surfaces 48h earlier than it
    // otherwise would (the existing config keeps flowing until the timelock
    // elapses and the admin calls UpdateConfig, but a malformed proposal
    // should fail loudly now, not then).
    validate_factory_config(deps.as_ref(), &env, &config)?;

    let pending = PendingConfig {
        new_config: config,
        effective_after: env.block.time.plus_seconds(ADMIN_TIMELOCK_SECONDS),
    };
    PENDING_CONFIG.save(deps.storage, &pending)?;
    Ok(Response::new()
        .add_attribute("action", "propose_config_update")
        .add_attribute("effective_after", pending.effective_after.to_string()))
}

pub fn execute_cancel_factory_config_update(
    deps: DepsMut,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;
    PENDING_CONFIG.remove(deps.storage);
    Ok(Response::new().add_attribute("action", "cancel_config_update"))
}

pub fn execute_propose_pool_config_update(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    pool_id: u64,
    update_msg: PoolConfigUpdate,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;

    // Verify the pool exists before accepting a proposal for it.
    POOLS_BY_ID.load(deps.storage, pool_id).map_err(|_| {
        ContractError::Std(StdError::generic_err(format!(
            "Pool {} not found in registry",
            pool_id
        )))
    })?;

    if PENDING_POOL_CONFIG
        .may_load(deps.storage, pool_id)?
        .is_some()
    {
        return Err(ContractError::Std(StdError::generic_err(
            "A pool config update is already pending for this pool. Cancel it first.",
        )));
    }

    // Propose-time bound check. Mirrors `pool_core`'s apply-time validation
    // so an out-of-range value fails immediately rather than after the
    // 48h timelock (where the pool would reject and the admin would have to
    // Cancel + re-Propose + wait another 48h).
    update_msg.validate()?;

    let effective_after = env.block.time.plus_seconds(ADMIN_TIMELOCK_SECONDS);

    PENDING_POOL_CONFIG.save(
        deps.storage,
        pool_id,
        &PendingPoolConfig {
            pool_id,
            update: update_msg,
            effective_after,
        },
    )?;

    Ok(Response::new()
        .add_attribute("action", "propose_pool_config_update")
        .add_attribute("pool_id", pool_id.to_string())
        .add_attribute("effective_after", effective_after.to_string()))
}

pub fn execute_apply_pool_config_update(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    pool_id: u64,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;

    let pending = PENDING_POOL_CONFIG
        .load(deps.storage, pool_id)
        .map_err(|_| {
            ContractError::Std(StdError::generic_err(
                "No pending pool config update for this pool",
            ))
        })?;

    if env.block.time < pending.effective_after {
        return Err(ContractError::TimelockNotExpired {
            effective_after: pending.effective_after,
        });
    }

    // Re-validate at apply time. Bounds are static today, but pool-core's
    // bounds could plausibly tighten in a future migration between propose
    // and apply; re-checking here keeps the factory's behaviour aligned
    // with whatever the live build accepts. Cheap to run.
    pending.update.validate()?;

    let pool_addr = POOLS_BY_ID.load(deps.storage, pool_id)?.creator_pool_addr;

    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum PoolExecuteMsg {
        UpdateConfigFromFactory { update: PoolConfigUpdate },
    }
    let msg = CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: pool_addr.to_string(),
        msg: to_json_binary(&PoolExecuteMsg::UpdateConfigFromFactory {
            update: pending.update,
        })?,
        funds: vec![],
    });

    PENDING_POOL_CONFIG.remove(deps.storage, pool_id);

    Ok(Response::new()
        .add_message(msg)
        .add_attribute("action", "execute_pool_config_update")
        .add_attribute("pool_id", pool_id.to_string()))
}

pub fn execute_cancel_pool_config_update(
    deps: DepsMut,
    info: MessageInfo,
    pool_id: u64,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info)?;

    if PENDING_POOL_CONFIG
        .may_load(deps.storage, pool_id)?
        .is_none()
    {
        return Err(ContractError::Std(StdError::generic_err(
            "No pending pool config update to cancel",
        )));
    }

    PENDING_POOL_CONFIG.remove(deps.storage, pool_id);

    Ok(Response::new()
        .add_attribute("action", "cancel_pool_config_update")
        .add_attribute("pool_id", pool_id.to_string()))
}
