//! Pool contract entry points: instantiate, execute dispatch, swap, migrate.
//!
//! Commit logic lives in [`crate::commit`], admin operations in [`crate::admin`].
//!
//! Phase-2: the internal AMM + LP system is gone. Swaps and post-threshold
//! commits route through the NATIVE Osmosis pool via `MsgSwapExactAmountIn`
//! and forward the output in a reply; the pool is seeded at threshold
//! crossing via `MsgCreateBalancerPool` whose reply records the native
//! `pool_id`.

use crate::admin::{
    ensure_not_drained, execute_cancel_emergency_withdraw, execute_claim_failed_distribution,
    execute_emergency_withdraw, execute_pause, execute_recover_stuck_states,
    execute_self_recover_distribution, execute_unpause, execute_update_config_from_factory,
};
use crate::asset::{PoolPairType, TokenInfoPoolExt, TokenType};
use crate::commit::{commit, execute_continue_distribution};
use crate::error::ContractError;
use crate::generic_helpers::validate_pool_threshold_payments;
use crate::liquidity_helpers::execute_claim_creator_excess;
use crate::msg::{ExecuteMsg, MigrateMsg, PoolInstantiateMsg};
use crate::query::query_check_commit;
use crate::state::{
    CommitLimitInfo, ExpectedFactory, PoolAnalytics, PoolDetails, PoolInfo, PoolSpecs, PoolState,
    ThresholdPayoutAmounts, COMMITFEEINFO, COMMIT_LIMIT_INFO, DEFAULT_LP_FEE,
    DEFAULT_SWAP_RATE_LIMIT_SECS, EXPECTED_FACTORY, FAILED_MINTS, IS_THRESHOLD_HIT, MAX_LP_FEE,
    MIN_LP_FEE, NATIVE_RAISED_FROM_COMMIT, PENDING_FACTORY_NOTIFY, PENDING_MINT_REPLIES,
    POOL_ANALYTICS, POOL_ID, POOL_INFO, POOL_PAUSED, POOL_SPECS, POOL_STATE,
    REPLY_ID_CREATE_POOL, REPLY_ID_DISTRIBUTION_MINT_BASE, REPLY_ID_FACTORY_NOTIFY_INITIAL,
    REPLY_ID_FACTORY_NOTIFY_RETRY, REPLY_ID_SWAP_FORWARD, THRESHOLD_PAYOUT_AMOUNTS,
    USD_RAISED_FROM_COMMIT,
};
use crate::swap_helper::simple_swap;
use cosmwasm_std::{
    entry_point, from_json, to_json_binary, Addr, CosmosMsg, Decimal, DepsMut, Env, MessageInfo,
    Reply, Response, StdError, StdResult, Storage, SubMsg, SubMsgResult, Uint128, WasmMsg,
};
use cw2::set_contract_version;
use osmosis_std::types::osmosis::gamm::poolmodels::balancer::v1beta1::MsgCreateBalancerPoolResponse;
use prost::Message;

/// cw2 contract name.
const CONTRACT_NAME: &str = "bluechip-osmosis-creator-pool";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Protobuf type URL for the GAMM create-balancer-pool response.
const CREATE_BALANCER_POOL_RESPONSE_TYPE: &str =
    "/osmosis.gamm.poolmodels.balancer.v1beta1.MsgCreateBalancerPoolResponse";

// ---------------------------------------------------------------------------
// Instantiate
// ---------------------------------------------------------------------------

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: PoolInstantiateMsg,
) -> Result<Response, ContractError> {
    let cfg = ExpectedFactory {
        expected_factory_address: msg.used_factory_addr.clone(),
    };
    EXPECTED_FACTORY.save(deps.storage, &cfg)?;
    if info.sender != cfg.expected_factory_address {
        return Err(ContractError::Unauthorized {});
    }

    // Enforce strict pair shape AND ordering: index 0 = Bluechip (Native),
    // index 1 = CreatorToken placeholder.
    let bluechip_side = msg.pool_token_info[0].clone();
    match &bluechip_side {
        TokenType::Native { denom } if !denom.trim().is_empty() => {}
        TokenType::Native { .. } => {
            return Err(ContractError::InvalidPairShape {
                reason: "Bluechip denom must be non-empty".to_string(),
            });
        }
        _ => {
            return Err(ContractError::InvalidPairShape {
                reason: "pool_token_info[0] must be the Bluechip(Native) side — order \
                         matters: bluechip at index 0, creator-token at index 1."
                    .to_string(),
            });
        }
    }
    if !matches!(msg.pool_token_info[1], TokenType::CreatorToken { .. }) {
        return Err(ContractError::InvalidPairShape {
            reason: "pool_token_info[1] must be the CreatorToken placeholder — order \
                     matters: bluechip at index 0, creator-token at index 1."
                .to_string(),
        });
    }
    if msg.subdenom.trim().is_empty() {
        return Err(ContractError::InvalidPairShape {
            reason: "subdenom must be non-empty".to_string(),
        });
    }

    // Deterministic creator-token denom: the pool is `admin`.
    let creator_denom = pool_core::osmosis_msgs::full_denom(&env.contract.address, &msg.subdenom);
    let creator_side = TokenType::CreatorToken {
        denom: creator_denom.clone(),
    };
    let pool_token_info: [TokenType; 2] = [bluechip_side, creator_side];
    pool_token_info[0].check(deps.api)?;
    pool_token_info[1].check(deps.api)?;
    if pool_token_info[0] == pool_token_info[1] {
        return Err(ContractError::DoublingAssets {});
    }

    // Register the TokenFactory denom with the pool as admin.
    let create_denom =
        pool_core::osmosis_msgs::create_denom_msg(&env.contract.address, &msg.subdenom);

    if (msg.commit_fee_info.commit_fee_bluechip + msg.commit_fee_info.commit_fee_creator)
        > Decimal::one()
    {
        return Err(ContractError::InvalidFee {});
    }

    let threshold_payout_amounts = if let Some(params_binary) = msg.threshold_payout {
        let params: ThresholdPayoutAmounts = from_json(params_binary)?;
        validate_pool_threshold_payments(&params)?;
        params
    } else {
        return Err(ContractError::InvalidThresholdParams {
            msg: "Your params could not be validated during pool instantiation.".to_string(),
        });
    };

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let pool_info = PoolInfo {
        pool_id: msg.pool_id,
        pool_info: PoolDetails {
            contract_addr: env.contract.address.clone(),
            asset_infos: pool_token_info.clone(),
            pool_type: PoolPairType::Xyk {},
        },
        factory_addr: msg.used_factory_addr.clone(),
        token_denom: creator_denom.clone(),
    };

    let pool_specs = PoolSpecs {
        lp_fee: DEFAULT_LP_FEE,
        min_commit_interval: DEFAULT_SWAP_RATE_LIMIT_SECS,
    };

    let commit_config = CommitLimitInfo {
        commit_amount_for_threshold_usd: msg.commit_threshold_limit_usd,
        max_bluechip_lock_per_pool: msg.max_bluechip_lock_per_pool,
        creator_excess_liquidity_lock_days: msg.creator_excess_liquidity_lock_days,
        min_commit_usd_pre_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_PRE_THRESHOLD,
        min_commit_usd_post_threshold: crate::state::DEFAULT_MIN_COMMIT_USD_POST_THRESHOLD,
    };

    let pool_state = PoolState {
        pool_contract_address: env.contract.address.clone(),
    };

    USD_RAISED_FROM_COMMIT.save(deps.storage, &Uint128::zero())?;
    COMMITFEEINFO.save(deps.storage, &msg.commit_fee_info)?;
    NATIVE_RAISED_FROM_COMMIT.save(deps.storage, &Uint128::zero())?;
    IS_THRESHOLD_HIT.save(deps.storage, &false)?;
    POOL_INFO.save(deps.storage, &pool_info)?;
    POOL_STATE.save(deps.storage, &pool_state)?;
    POOL_SPECS.save(deps.storage, &pool_specs)?;
    THRESHOLD_PAYOUT_AMOUNTS.save(deps.storage, &threshold_payout_amounts)?;
    COMMIT_LIMIT_INFO.save(deps.storage, &commit_config)?;
    POOL_ANALYTICS.save(deps.storage, &PoolAnalytics::default())?;

    Ok(Response::new()
        .add_message(create_denom)
        .add_attribute("action", "instantiate")
        .add_attribute("pool_kind", crate::state::POOL_KIND_COMMIT)
        .add_attribute("pool_contract", env.contract.address.to_string())
        .add_attribute("token_denom", creator_denom))
}

// ---------------------------------------------------------------------------
// Execute dispatch
// ---------------------------------------------------------------------------

fn check_pool_not_paused(storage: &dyn Storage) -> Result<(), ContractError> {
    if POOL_PAUSED.may_load(storage)?.unwrap_or(false) {
        return Err(ContractError::PoolPausedLowLiquidity {});
    }
    Ok(())
}

/// Strict gate: hard-rejects whenever the pool is paused for ANY reason
/// and rejects when permanently drained. Used by the creator claim path.
fn check_pool_writable(storage: &dyn Storage) -> Result<(), ContractError> {
    ensure_not_drained(storage)?;
    check_pool_not_paused(storage)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        // --- Admin ---
        ExecuteMsg::UpdateConfigFromFactory { update } => {
            execute_update_creator_config_from_factory(deps, env, info, update)
        }
        ExecuteMsg::Pause {} => execute_pause(deps, env, info),
        ExecuteMsg::Unpause {} => execute_unpause(deps, env, info),
        ExecuteMsg::EmergencyWithdraw {} => execute_emergency_withdraw(deps, env, info),
        ExecuteMsg::CancelEmergencyWithdraw {} => {
            execute_cancel_emergency_withdraw(deps, env, info)
        }
        ExecuteMsg::RecoverStuckStates { recovery_type } => {
            execute_recover_stuck_states(deps, env, info, recovery_type)
        }

        // --- Commit & Distribution ---
        ExecuteMsg::Commit {
            asset,
            transaction_deadline,
            belief_price,
            max_spread,
        } => {
            check_pool_not_paused(deps.storage)?;
            commit(
                deps,
                env,
                info,
                asset,
                transaction_deadline,
                belief_price,
                max_spread,
            )
        }
        ExecuteMsg::ContinueDistribution {} => execute_continue_distribution(deps, env, info),

        // --- Swap (routes through the native pool via reply) ---
        ExecuteMsg::SimpleSwap {
            offer_asset,
            belief_price,
            max_spread,
            allow_high_max_spread,
            to,
            transaction_deadline,
        } => {
            if !query_check_commit(deps.as_ref())? {
                return Err(ContractError::ShortOfThreshold {});
            }
            offer_asset.confirm_sent_native_balance(&info)?;
            let sender_addr = info.sender.clone();
            let to_addr: Option<Addr> = to
                .map(|to_str| deps.api.addr_validate(&to_str))
                .transpose()?;
            simple_swap(
                deps,
                env,
                info,
                sender_addr,
                offer_asset,
                belief_price,
                max_spread,
                allow_high_max_spread,
                to_addr,
                transaction_deadline,
                None,
            )
        }

        // --- Creator claims ---
        ExecuteMsg::ClaimCreatorExcessLiquidity {
            transaction_deadline,
        } => {
            check_pool_writable(deps.storage)?;
            execute_claim_creator_excess(deps, env, info, transaction_deadline)
        }

        ExecuteMsg::RetryFactoryNotify {} => execute_retry_factory_notify(deps, env, info),
        ExecuteMsg::SelfRecoverDistribution {} => {
            execute_self_recover_distribution(deps, env, info)
        }
        ExecuteMsg::ClaimFailedDistribution { recipient } => {
            execute_claim_failed_distribution(deps, env, info, recipient)
        }
    }
}

/// Creator-pool wrapper around pool-core's
/// `execute_update_config_from_factory`. Applies the creator-pool-only
/// commit-floor knobs, then delegates the shared knobs.
fn execute_update_creator_config_from_factory(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    update: crate::msg::PoolConfigUpdate,
) -> Result<Response, ContractError> {
    use crate::state::MAX_MIN_COMMIT_USD;

    let pool_info = POOL_INFO.load(deps.storage)?;
    if info.sender != pool_info.factory_addr {
        return Err(ContractError::Unauthorized {});
    }

    let pre = update.min_commit_usd_pre_threshold;
    let post = update.min_commit_usd_post_threshold;

    if pre.is_some() || post.is_some() {
        let mut commit_config = COMMIT_LIMIT_INFO.load(deps.storage)?;
        if let Some(v) = pre {
            if v.is_zero() || v > MAX_MIN_COMMIT_USD {
                return Err(ContractError::InvalidCommitFloor {
                    field: "min_commit_usd_pre_threshold",
                    got: v,
                    max: MAX_MIN_COMMIT_USD,
                });
            }
            commit_config.min_commit_usd_pre_threshold = v;
        }
        if let Some(v) = post {
            if v.is_zero() || v > MAX_MIN_COMMIT_USD {
                return Err(ContractError::InvalidCommitFloor {
                    field: "min_commit_usd_post_threshold",
                    got: v,
                    max: MAX_MIN_COMMIT_USD,
                });
            }
            commit_config.min_commit_usd_post_threshold = v;
        }
        COMMIT_LIMIT_INFO.save(deps.storage, &commit_config)?;
    }

    execute_update_config_from_factory(deps.branch(), env, info, update)
}

/// Re-sends `NotifyThresholdCrossed` to the factory when the initial
/// notification failed. Permissionless; the factory's idempotency check
/// gates double-processing.
pub fn execute_retry_factory_notify(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
) -> Result<Response, ContractError> {
    let pending = PENDING_FACTORY_NOTIFY
        .may_load(deps.storage)?
        .unwrap_or(false);
    if !pending {
        return Err(ContractError::NoPendingFactoryNotify);
    }

    let pool_info = POOL_INFO.load(deps.storage)?;
    let crossed_at = crate::state::THRESHOLD_CROSSED_AT.load(deps.storage)?;
    let notify = SubMsg::reply_always(
        CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: pool_info.factory_addr.to_string(),
            msg: to_json_binary(
                &pool_factory_interfaces::FactoryExecuteMsg::NotifyThresholdCrossed {
                    pool_id: pool_info.pool_id,
                    crossed_at: Some(crossed_at),
                },
            )?,
            funds: vec![],
        }),
        REPLY_ID_FACTORY_NOTIFY_RETRY,
    );

    Ok(Response::new()
        .add_submessage(notify)
        .add_attribute("action", "retry_factory_notify")
        .add_attribute("pool_id", pool_info.pool_id.to_string())
        .add_attribute("crossed_at", crossed_at.to_string()))
}

/// Parse `MsgCreateBalancerPoolResponse.pool_id` from the create-pool
/// reply, preferring the typed `msg_responses` entry and falling back to
/// the deprecated `data` field.
fn parse_created_pool_id(result: &SubMsgResult) -> StdResult<u64> {
    let response = result.clone().into_result().map_err(StdError::generic_err)?;
    let bytes: Vec<u8> = response
        .msg_responses
        .iter()
        .find(|r| r.type_url == CREATE_BALANCER_POOL_RESPONSE_TYPE)
        .map(|r| r.value.to_vec())
        .or_else(|| {
            #[allow(deprecated)]
            response.data.as_ref().map(|d| d.to_vec())
        })
        .ok_or_else(|| {
            StdError::generic_err("create-pool reply: no MsgCreateBalancerPoolResponse in reply")
        })?;
    let decoded = MsgCreateBalancerPoolResponse::decode(bytes.as_slice())
        .map_err(|e| StdError::generic_err(format!("create-pool reply: decode failed: {}", e)))?;
    Ok(decoded.pool_id)
}

/// SubMsg reply handler.
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    match msg.id {
        REPLY_ID_CREATE_POOL => {
            // The native GAMM balancer pool was created at threshold
            // crossing; record its id so swaps / post-threshold commits can
            // route `MsgSwapExactAmountIn` through it.
            let pool_id = parse_created_pool_id(&msg.result)?;
            POOL_ID.save(deps.storage, &pool_id)?;
            Ok(Response::new()
                .add_attribute("action", "native_pool_created")
                .add_attribute("pool_id", pool_id.to_string())
                .add_attribute("block_time", env.block.time.seconds().to_string()))
        }
        REPLY_ID_SWAP_FORWARD => pool_core::swap::handle_swap_forward_reply(deps, env, msg),
        REPLY_ID_FACTORY_NOTIFY_INITIAL => {
            let err = match msg.result {
                SubMsgResult::Err(e) => e,
                SubMsgResult::Ok(_) => return Ok(Response::new()),
            };
            PENDING_FACTORY_NOTIFY.save(deps.storage, &true)?;
            Ok(Response::new()
                .add_attribute("action", "factory_notify_deferred")
                .add_attribute("reason", err)
                .add_attribute("block_time", env.block.time.seconds().to_string()))
        }
        REPLY_ID_FACTORY_NOTIFY_RETRY => match msg.result {
            SubMsgResult::Ok(_) => {
                PENDING_FACTORY_NOTIFY.save(deps.storage, &false)?;
                Ok(Response::new()
                    .add_attribute("action", "factory_notify_retry_succeeded")
                    .add_attribute("block_time", env.block.time.seconds().to_string()))
            }
            SubMsgResult::Err(e) => Ok(Response::new()
                .add_attribute("action", "factory_notify_retry_failed")
                .add_attribute("reason", e)
                .add_attribute("block_time", env.block.time.seconds().to_string())),
        },
        id if id >= REPLY_ID_DISTRIBUTION_MINT_BASE
            && PENDING_MINT_REPLIES.has(deps.storage, id) =>
        {
            let pending = PENDING_MINT_REPLIES
                .load(deps.storage, msg.id)
                .map_err(|e| {
                    StdError::generic_err(format!(
                        "distribution-mint reply load failed for id {}: {}",
                        msg.id, e
                    ))
                })?;
            PENDING_MINT_REPLIES.remove(deps.storage, msg.id);

            match msg.result {
                SubMsgResult::Ok(_) => Ok(Response::new()
                    .add_attribute("action", "distribution_mint_succeeded")
                    .add_attribute("user", pending.user.to_string())
                    .add_attribute("amount", pending.amount.to_string())
                    .add_attribute("reply_id", msg.id.to_string())),
                SubMsgResult::Err(e) => {
                    FAILED_MINTS.update::<_, StdError>(deps.storage, &pending.user, |existing| {
                        let prior = existing.unwrap_or_default();
                        prior
                            .checked_add(pending.amount)
                            .map_err(|o| StdError::generic_err(o.to_string()))
                    })?;
                    Ok(Response::new()
                        .add_attribute("action", "distribution_mint_isolated_failure")
                        .add_attribute("user", pending.user.to_string())
                        .add_attribute("amount", pending.amount.to_string())
                        .add_attribute("reply_id", msg.id.to_string())
                        .add_attribute("reason", e)
                        .add_attribute("block_time", env.block.time.seconds().to_string()))
                }
            }
        }
        other => Err(StdError::generic_err(
            pool_core::generic::unknown_reply_id_msg(pool_core::state::POOL_KIND_COMMIT, other),
        )),
    }
}

// ---------------------------------------------------------------------------
// Migrate
// ---------------------------------------------------------------------------

#[entry_point]
pub fn migrate(deps: DepsMut, _env: Env, msg: MigrateMsg) -> Result<Response, ContractError> {
    // Reject downgrades.
    if let Ok(stored_version) = cw2::get_contract_version(deps.storage) {
        let stored_semver: semver::Version =
            stored_version.version.parse().map_err(|e: semver::Error| {
                ContractError::StoredVersionInvalid {
                    version: stored_version.version.clone(),
                    msg: e.to_string(),
                }
            })?;
        let current_semver: semver::Version =
            CONTRACT_VERSION.parse().map_err(|e: semver::Error| {
                ContractError::CurrentVersionInvalid {
                    version: CONTRACT_VERSION.to_string(),
                    msg: e.to_string(),
                }
            })?;
        if stored_semver > current_semver {
            return Err(ContractError::DowngradeRefused {
                stored: stored_semver.to_string(),
                current: current_semver.to_string(),
            });
        }
    }

    match msg {
        MigrateMsg::UpdateFees { new_fees } => {
            if new_fees > MAX_LP_FEE || new_fees < MIN_LP_FEE {
                return Err(ContractError::LpFeeOutOfRange {
                    got: new_fees,
                    min: MIN_LP_FEE,
                    max: MAX_LP_FEE,
                });
            }
            POOL_SPECS.update(deps.storage, |mut specs| -> StdResult<_> {
                specs.lp_fee = new_fees;
                Ok(specs)
            })?;
        }
        MigrateMsg::UpdateVersion {} => {}
    }

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    Ok(Response::new()
        .add_attribute("action", "migrate")
        .add_attribute("version", CONTRACT_VERSION))
}
