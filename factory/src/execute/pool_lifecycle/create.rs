//! Pool creation entry point, plus the input
//! validators that guard it.
//!
//! Commit pools mint a fresh CW20 at creation and register through the
//! shared reply-ID / register_pool plumbing downstream.

use cosmwasm_std::{
    to_json_binary, CosmosMsg, DepsMut, Env, MessageInfo, Response, StdError, SubMsg, Uint128,
    WasmMsg,
};
use cw20::MinterResponse;
use cw_utils::{must_pay, PaymentError};

use crate::error::ContractError;
use crate::msg::{CreatorTokenInfo, TokenInstantiateMsg};
use crate::pool_struct::{CreatePool, TempPoolCreation};
use crate::state::{
    CreationStatus, PoolCreationContext, PoolCreationState, COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS,
    FACTORYINSTANTIATEINFO, LAST_COMMIT_POOL_CREATE_AT, POOL_COUNTER, POOL_CREATION_CONTEXT,
};

use super::super::{encode_reply_id, SET_TOKENS};

// Sentinel placeholder the caller must supply for the CreatorToken slot.
// The factory mints a fresh CW20 during pool creation and rewrites this
// entry to the real address in mint_create_pool. Any other value in the
// CreatorToken slot is rejected so attackers can't smuggle an arbitrary
// (possibly malicious) CW20 into the pool's asset_infos.
pub const CREATOR_TOKEN_SENTINEL: &str = "WILL_BE_CREATED_BY_FACTORY";

/// Validates the pair shape supplied by the commit-pool creator:
/// - exactly one Bluechip entry whose denom equals the factory's canonical
/// `bluechip_denom` (prevents attackers from registering pools under a
/// fake native denom they control via tokenfactory or similar)
/// - exactly one CreatorToken entry whose contract_addr equals the sentinel
///
/// Anything else (duplicate Bluechips with different denoms, two CreatorTokens,
/// a CreatorToken pointing at some pre-existing CW20, a Bluechip with a wrong
/// denom) is rejected up front so the downstream instantiate doesn't have to
/// untangle a malformed pair.
pub(crate) fn validate_pool_token_info(
    pool_token_info: &[crate::asset::TokenType; 2],
    canonical_bluechip_denom: &str,
) -> Result<(), ContractError> {
    use crate::asset::TokenType;

    // Strict ordering: bluechip MUST be at index 0, creator-token at
    // index 1. Every downstream piece of pool code (post_threshold_commit,
    // simple_swap, threshold_payout reserves) hard-codes the assumption
    // that `reserve0` is bluechip and `reserve1` is creator-token. The
    // factory's `mint_create_pool` rewrites the sentinel in place
    // preserving order, so a `[CreatorToken sentinel, Bluechip]` input
    // would propagate a reversed pair into the pool and silently produce
    // wrong-direction swaps. Enforcing order here keeps the assumption
    // load-bearing rather than incidental.
    match (&pool_token_info[0], &pool_token_info[1]) {
        (TokenType::Native { denom }, TokenType::CreatorToken { contract_addr }) => {
            if denom.trim().is_empty() {
                return Err(ContractError::Std(StdError::generic_err(
                    "Bluechip denom must be non-empty",
                )));
            }
            if denom != canonical_bluechip_denom {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "Bluechip denom must match the factory canonical denom \"{}\"; got \"{}\"",
                    canonical_bluechip_denom, denom
                ))));
            }
            if contract_addr.as_str() != CREATOR_TOKEN_SENTINEL {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "CreatorToken contract_addr must be the sentinel \"{}\"; got \"{}\". The factory mints the CW20 itself and rewrites this field.",
                    CREATOR_TOKEN_SENTINEL, contract_addr
                ))));
            }
            Ok(())
        }
        _ => Err(ContractError::Std(StdError::generic_err(
            "pool_token_info must be [Bluechip(canonical denom), CreatorToken(sentinel)] — \
             order matters: bluechip at index 0, creator-token at index 1.",
        ))),
    }
}

/// Validates creator token metadata before any state is written.
/// - decimals must be 6 (threshold payout and mint cap are calibrated for 6-decimal tokens)
/// - name: 3-50 chars, printable ASCII only (no control chars, no extended unicode)
/// - symbol: 3-12 chars, uppercase ASCII letters and digits only (matches cw20-base spec)
pub(crate) fn validate_creator_token_info(
    token_info: &CreatorTokenInfo,
) -> Result<(), ContractError> {
    if token_info.decimal != 6 {
        return Err(ContractError::Std(StdError::generic_err(
            "Token decimals must be 6. Threshold payout amounts and mint caps are calibrated for 6-decimal tokens.",
        )));
    }

    let name_len = token_info.name.chars().count();
    if !(3..=50).contains(&name_len) {
        return Err(ContractError::Std(StdError::generic_err(
            "Token name must be between 3 and 50 characters",
        )));
    }
    if !token_info
        .name
        .chars()
        .all(|c| c.is_ascii() && !c.is_ascii_control())
    {
        return Err(ContractError::Std(StdError::generic_err(
            "Token name must contain only printable ASCII characters",
        )));
    }

    let symbol_len = token_info.symbol.chars().count();
    if !(3..=12).contains(&symbol_len) {
        return Err(ContractError::Std(StdError::generic_err(
            "Token symbol must be between 3 and 12 characters",
        )));
    }
    if !token_info
        .symbol
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err(ContractError::Std(StdError::generic_err(
            "Token symbol must contain only uppercase ASCII letters (A-Z) and digits (0-9)",
        )));
    }
    // Require at least one letter. Pure-digit symbols ("123", "001")
    // pass the character-class check above but render as malformed in
    // most CW20 frontends and confuse human readers (looks like a token
    // ID, not a ticker). Mainline tickers are letters + optional digits;
    // gating on ≥1 letter rules out the cosmetic-bug shape without
    // restricting legitimate naming.
    if !token_info.symbol.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(ContractError::Std(StdError::generic_err(
            "Token symbol must contain at least one uppercase ASCII letter (A-Z); \
             all-digit symbols are not allowed",
        )));
    }

    Ok(())
}

pub(crate) fn execute_create_creator_pool(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    pool_msg: CreatePool,
    token_info: CreatorTokenInfo,
) -> Result<Response, ContractError> {
    // Validate token metadata and pair shape up front, before any state
    // writes. These checks must stay at the top of the handler — they
    // guard every later step of pool creation.
    validate_creator_token_info(&token_info)?;
    let factory_cw20 = FACTORYINSTANTIATEINFO.load(deps.storage)?;
    validate_pool_token_info(&pool_msg.pool_token_info, &factory_cw20.bluechip_denom)?;

    // Per-address rate limit. Reject if `info.sender` already
    // created a commit pool within the last
    // COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS. Stamps the new timestamp
    // before any SubMsg dispatch, so a failed reply chain (which
    // reverts the whole tx atomically) also reverts the stamp —
    // no permanent rate-limit state leaks from failed creates.
    //
    // Runs BEFORE the fee/funds check so a rate-limited
    // caller sees the rate-limit error directly rather than a
    // misleading "insufficient fee" error (when the actual block
    // is the cooldown, not the fee).
    let now = env.block.time;
    let prior_stamp = LAST_COMMIT_POOL_CREATE_AT.may_load(deps.storage, info.sender.clone())?;
    if let Some(last) = prior_stamp {
        let next_allowed = last.plus_seconds(COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS);
        if now < next_allowed {
            return Err(ContractError::Std(StdError::generic_err(format!(
                "Rate-limited: this address can create another commit pool after {} \
                 (last create at {}, cooldown {}s)",
                next_allowed, last, COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS
            ))));
        }
    }
    LAST_COMMIT_POOL_CREATE_AT.save(deps.storage, info.sender.clone(), &now)?;
    // Sync the timestamp-ordered secondary index used by PruneRateLimits.
    // Remove the prior (old_ts, addr) entry first so the index stays
    // single-entry-per-address; the index is keyed by timestamp so an
    // un-removed prior would persist as a stale ghost.
    if let Some(prior) = prior_stamp {
        crate::state::COMMIT_POOL_CREATE_TS_INDEX
            .remove(deps.storage, (prior.seconds(), info.sender.clone()));
    }
    crate::state::COMMIT_POOL_CREATE_TS_INDEX.save(
        deps.storage,
        (now.seconds(), info.sender.clone()),
        &(),
    )?;

    // Charge a flat creation fee (denominated in the chain's native
    // asset) for pool creation as anti-spam friction. Deployments can
    // enable/disable it
    // from a single config value. Zero disables the fee entirely.
    let required_bluechip = factory_cw20.pool_creation_fee;
    let fee_source = if required_bluechip.is_zero() {
        "disabled"
    } else {
        "config"
    };
    // Strict single-denom funds validation` + refund-extras pattern with `must_pay`).
    // `must_pay` enforces that `info.funds` contains exactly one Coin
    // entry whose denom equals `bluechip_denom` and whose amount is
    // non-zero; any other shape (multi-denom, wrong denom, empty, zero
    // amount) errors out and the tx reverts. On revert the bank module
    // auto-returns all attached funds to the caller — no in-tx refund
    // path required for non-bluechip denoms, which closes the
    // "extra-funds-attached" griefing vector.
    //
    // Two-mode behavior keyed off the live fee:
    // - Fee enabled (`required_bluechip > 0`): use `must_pay`. Surplus
    // over `required_bluechip` is refunded in the same tx.
    // - Fee disabled (`required_bluechip == 0`): no funds are expected
    // and none are accepted. Any attached funds (even bluechip)
    // error out — callers who paid by mistake get everything back on
    // revert. This is intentional: a disabled fee shouldn't quietly
    // accept then refund payments, because that masks frontend bugs.
    let paid_bluechip = if required_bluechip.is_zero() {
        if !info.funds.is_empty() {
            return Err(ContractError::Std(StdError::generic_err(
                "Commit-pool creation fee is disabled; do not attach any funds.",
            )));
        }
        Uint128::zero()
    } else {
        match must_pay(&info, &factory_cw20.bluechip_denom) {
            Ok(amount) => amount,
            Err(PaymentError::NoFunds {}) | Err(PaymentError::MissingDenom(_)) => {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "Insufficient commit-pool creation fee: required {} {}, paid 0 {}",
                    required_bluechip, factory_cw20.bluechip_denom, factory_cw20.bluechip_denom
                ))));
            }
            Err(e) => {
                return Err(ContractError::Std(StdError::generic_err(format!(
                    "Invalid commit-pool creation funds: {}. Send exactly one denom ({}).",
                    e, factory_cw20.bluechip_denom
                ))));
            }
        }
    };
    if paid_bluechip < required_bluechip {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "Insufficient commit-pool creation fee: required {} {}, paid {} {}",
            required_bluechip,
            factory_cw20.bluechip_denom,
            paid_bluechip,
            factory_cw20.bluechip_denom
        ))));
    }
    let surplus = paid_bluechip.checked_sub(required_bluechip)?;
    let mut fee_messages: Vec<CosmosMsg> = Vec::new();
    if !required_bluechip.is_zero() {
        fee_messages.push(CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: factory_cw20.bluechip_wallet_address.to_string(),
            amount: vec![cosmwasm_std::Coin {
                denom: factory_cw20.bluechip_denom.clone(),
                amount: required_bluechip,
            }],
        }));
    }
    if !surplus.is_zero() {
        fee_messages.push(CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: info.sender.to_string(),
            amount: vec![cosmwasm_std::Coin {
                denom: factory_cw20.bluechip_denom.clone(),
                amount: surplus,
            }],
        }));
    }

    let creator_attr = info.sender.to_string();
    let pool_counter = POOL_COUNTER.may_load(deps.storage)?.unwrap_or(0);
    let pool_id = pool_counter + 1;
    POOL_COUNTER.save(deps.storage, &pool_id)?;

    let msg = WasmMsg::Instantiate {
        code_id: factory_cw20.cw20_token_contract_id,
        //creating the creator token only, no minting.
        msg: to_json_binary(&TokenInstantiateMsg {
            name: token_info.name.clone(),
            symbol: token_info.symbol,
            decimals: token_info.decimal,
            initial_balances: vec![],
            mint: Some(MinterResponse {
                minter: env.contract.address.to_string(),
                // Mint cap pinned to the exact threshold-payout total
                // derived from `factory_cw20.threshold_payout_amounts`
                // (default: creator 325e9 + bluechip 25e9 + pool_seed
                // 350e9 + commit_return 500e9 = 1.2e12). No protocol
                // path ever needs to mint beyond this — the payout is
                // fixed at threshold-cross and validated by
                // `ThresholdPayoutAmounts::validate` (propose-time)
                // and `validate_pool_threshold_payments` (runtime).
                // If any future code path ever gained mint authority
                // and tried to mint extra tokens, cw20-base would
                // reject the mint and revert the entire tx
                // (fail-closed) rather than silently letting
                // additional supply be created.
                cap: Some(factory_cw20.threshold_payout_amounts.total_mint()?),
            }),
            // Marketing admin = the pool creator. cw20-base permanently
            // locks marketing when this is None at instantiation, which
            // would leave every creator token unable to ever set a logo,
            // description, or project URL. Project/description/logo start
            // empty; the creator fills them in via UpdateMarketing /
            // UploadLogo on the token contract.
            marketing: Some(crate::msg::InstantiateMarketingInfo {
                project: None,
                description: None,
                marketing: Some(info.sender.to_string()),
                logo: None,
            }),
        })?,
        //no initial balance. waits until threshold is crossed to mint creator tokens.
        funds: vec![],
        admin: Some(env.contract.address.to_string()),
        label: token_info.name,
    };
    POOL_CREATION_CONTEXT.save(
        deps.storage,
        pool_id,
        &PoolCreationContext {
            temp: TempPoolCreation {
                temp_pool_info: pool_msg,
                temp_creator_wallet: info.sender.clone(),
                pool_id,
                creator_token_addr: None,
                nft_addr: None,
            },
            state: PoolCreationState {
                pool_id,
                creator: info.sender,
                creation_time: env.block.time,
                status: CreationStatus::Started,
            },
        },
    )?;
    let sub_msg = vec![SubMsg::reply_on_success(
        msg,
        encode_reply_id(pool_id, SET_TOKENS),
    )];

    Ok(Response::new()
        .add_messages(fee_messages)
        .add_attribute("action", "create")
        .add_attribute("creator", creator_attr)
        .add_attribute("pool_id", pool_id.to_string())
        .add_attribute("required_fee_bluechip", required_bluechip.to_string())
        .add_attribute("paid_fee_bluechip", paid_bluechip.to_string())
        .add_attribute("refunded_bluechip", surplus.to_string())
        .add_attribute("fee_source", fee_source)
        .add_submessages(sub_msg))
}
