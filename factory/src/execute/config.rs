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
/// Fields that may be updated WITHOUT re-running the live Pyth probe. The
/// probe's result is independent of every one of them — it reads only
/// `pyth_contract_addr`, `pyth_native_usd_feed_id`, `max_pyth_staleness_seconds`
/// and `pyth_conf_threshold_bps` — and each remains validated by the cheap,
/// always-run checks in `validate_factory_config`. Letting these through
/// without a fresh feed means a lapsed price keeper cannot block unrelated
/// admin actions (most importantly rotating a compromised
/// `bluechip_wallet_address`).
///
/// Returns true when `proposed` differs from `current` ONLY in these fields.
///
/// SAFETY: implemented by neutralizing exactly this operational allowlist and
/// then comparing the WHOLE struct, so ANY field not listed here — every field
/// the probe reads, the priced-asset / fee-swap route fields, and any field
/// added to `FactoryInstantiate` in the future — forces a re-probe by default.
/// NEVER add a pricing- or fee-route-relevant field to this list.
fn only_probe_independent_fields_changed(
    current: &FactoryInstantiate,
    proposed: &FactoryInstantiate,
) -> bool {
    let mut probe_view = proposed.clone();
    probe_view.factory_admin_address = current.factory_admin_address.clone();
    probe_view.bluechip_wallet_address = current.bluechip_wallet_address.clone();
    probe_view.commit_fee_bluechip = current.commit_fee_bluechip;
    probe_view.commit_fee_creator = current.commit_fee_creator;
    probe_view.commit_threshold_limit_usd = current.commit_threshold_limit_usd;
    probe_view.max_bluechip_lock_per_pool = current.max_bluechip_lock_per_pool;
    probe_view.creator_excess_liquidity_lock_days = current.creator_excess_liquidity_lock_days;
    probe_view.pool_creation_fee = current.pool_creation_fee;
    probe_view.threshold_payout_amounts = current.threshold_payout_amounts.clone();
    probe_view.emergency_withdraw_delay_seconds = current.emergency_withdraw_delay_seconds;
    probe_view.cw20_token_contract_id = current.cw20_token_contract_id;
    probe_view.cw721_nft_contract_id = current.cw721_nft_contract_id;
    probe_view.create_pool_wasm_contract_id = current.create_pool_wasm_contract_id;
    // Deliberately NOT copied (⇒ a change forces a re-probe): the probe inputs
    // pyth_contract_addr / pyth_native_usd_feed_id / max_pyth_staleness_seconds /
    // pyth_conf_threshold_bps, and the priced-asset / fee-swap route fields
    // bluechip_denom / pricing_pool_id / usd_quote_denom / gamm_pool_creation_fee.
    probe_view == *current
}

/// `current` is the config already stored on the factory (for propose/apply);
/// pass `None` at instantiate (no prior config, so the live probe always runs).
/// When `current` is `Some` and only probe-independent operational fields
/// changed, the live Pyth probe is skipped so a keeper outage can't block the
/// change — every other (cheap) validation below still runs unconditionally.
pub(crate) fn validate_factory_config(
    deps: cosmwasm_std::Deps,
    env: &Env,
    config: &FactoryInstantiate,
    current: Option<&FactoryInstantiate>,
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
    // Reject a 100%+ total fee: at fee_sum == 1.0 every commit is consumed
    // entirely by fees, leaving nothing toward the threshold, and the pool's
    // own instantiate rejects it as `InvalidFee` — bricking new pool creation
    // for a full 48h timelock cycle. Require strictly < 1.0.
    if fee_sum >= cosmwasm_std::Decimal::one() {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "commit_fee_bluechip + commit_fee_creator must be < 1.0; got {}",
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
    // Cross-denom fee-swap route. `pricing_pool_id` is NOT a price source
    // (USD valuation is via Pyth); it is only the pool used to acquire the
    // `usd_quote_denom`-denominated gamm creation fee at crossing. A broken
    // value here would brick crossings when the gamm fee is in that quote
    // denom, so validate at propose time.
    if config.pricing_pool_id == 0 {
        return Err(ContractError::Std(StdError::generic_err(
            "pricing_pool_id must be non-zero (the Osmosis pool that swaps bluechip_denom into usd_quote_denom for the gamm creation fee at crossing)",
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
    // --- Pyth oracle config validation ---
    if config.pyth_contract_addr.trim().is_empty() {
        return Err(ContractError::Std(StdError::generic_err(
            "pyth_contract_addr must be non-empty (the Pyth CW contract address)",
        )));
    }
    // A malformed/unreachable address is caught by the live Pyth probe
    // below (the smart query fails), so no separate addr_validate here —
    // that keeps the check chain-native rather than relying on the host
    // bech32 prefix.
    // Pyth feed ids are 64 lowercase hex chars (no `0x`).
    let feed = config.pyth_native_usd_feed_id.trim();
    if feed.len() != 64 || !feed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ContractError::Std(StdError::generic_err(
            "pyth_native_usd_feed_id must be 64 hex characters (no 0x prefix)",
        )));
    }
    if config.max_pyth_staleness_seconds < crate::usd_price::MAX_PYTH_STALENESS_MIN_SECONDS
        || config.max_pyth_staleness_seconds > crate::usd_price::MAX_PYTH_STALENESS_MAX_SECONDS
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "max_pyth_staleness_seconds {} outside allowed range [{}, {}]",
            config.max_pyth_staleness_seconds,
            crate::usd_price::MAX_PYTH_STALENESS_MIN_SECONDS,
            crate::usd_price::MAX_PYTH_STALENESS_MAX_SECONDS,
        ))));
    }
    if config.pyth_conf_threshold_bps < crate::usd_price::PYTH_CONF_THRESHOLD_BPS_MIN
        || config.pyth_conf_threshold_bps > crate::usd_price::PYTH_CONF_THRESHOLD_BPS_MAX
    {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "pyth_conf_threshold_bps {} outside allowed range [{}, {}]",
            config.pyth_conf_threshold_bps,
            crate::usd_price::PYTH_CONF_THRESHOLD_BPS_MIN,
            crate::usd_price::PYTH_CONF_THRESHOLD_BPS_MAX,
        ))));
    }

    // Live probe of the Pyth route. A typo'd contract/feed, a stale feed,
    // or a wide-confidence price would otherwise surface only as a
    // chain-wide commit outage after the 48h timelock. Reading the real
    // Pyth price against the proposed config turns it into an instant
    // instantiate/propose/apply-time error.
    //
    // The probe is skipped ONLY when this is a config UPDATE (`current` is
    // Some) whose changes are confined to probe-independent operational
    // fields. That decouples a lapsed price keeper from unrelated admin
    // actions: e.g. rotating a compromised `bluechip_wallet_address` must not
    // require a fresh feed. Any change touching the probe's inputs or the
    // pricing/fee-route fields still probes, and instantiate (`current` None)
    // always probes. All the cheap validations above/below run regardless.
    let must_probe = match current {
        None => true,
        Some(cur) => !only_probe_independent_fields_changed(cur, config),
    };
    if must_probe {
        crate::usd_price::probe_native_usd_rate(deps, env, config).map_err(|e| {
            ContractError::Std(StdError::generic_err(format!(
                "pricing config failed live Pyth probe (contract {}, feed {}): {}",
                config.pyth_contract_addr, config.pyth_native_usd_feed_id, e
            )))
        })?;
    }

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

    // The GAMM pool-creation-fee config. Two payable shapes exist:
    // - denom == bluechip_denom (osmo-test-5: 1 OSMO): the pool retains
    //   this much bluechip from the 1% commit fee and the gamm module
    //   charges it straight from the pool's native balance;
    // - denom == usd_quote_denom (osmosis-1: 20 Noble USDC): the pool
    //   still retains NATIVE from the 1% fee (sized at the live Pyth
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
    // drained or pruned, or the Pyth config could no longer read); re-running
    // the validation — including the live Pyth probe when the proposal touches
    // pricing — catches stale-proposal hazards before the state lands. The
    // still-stored config is the "current" baseline for the skip decision.
    let current = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    validate_factory_config(deps.as_ref(), &env, &pending.new_config, Some(&current))?;

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
    // should fail loudly now, not then). The stored config is the baseline
    // for deciding whether the live probe is needed.
    let current = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    validate_factory_config(deps.as_ref(), &env, &config, Some(&current))?;

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
