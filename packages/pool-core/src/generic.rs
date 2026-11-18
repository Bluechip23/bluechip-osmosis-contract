//! Shared primitives used by swap, liquidity, admin, and (via
//! creator-pool `generic_helpers`) the commit flow.
//!
//! Scope: anything that either touches only shared state
//! (POOL_FEE_STATE, POOL_STATE, USER_LAST_COMMIT) or is pure. Commit-
//! phase helpers — `trigger_threshold_payout`, `process_distribution_batch`,
//! `calculate_effective_batch_size`, `update_commit_info`, `mint_tokens`,
//! `validate_pool_threshold_payments` — stay in the creator-pool crate.

use crate::error::ContractError;
use crate::state::{PoolSpecs, USER_LAST_COMMIT, USER_LAST_LIQUIDITY_OP};
use cosmwasm_std::{
    Addr, BankMsg, Coin, CosmosMsg, Decimal, Decimal256, DepsMut, Env, StdError, StdResult,
    Timestamp, Uint128,
};
use cw_storage_plus::Map;

/// Swap / commit cooldown. Keyed in `USER_LAST_COMMIT` by the calling
/// address. It only spaces out DIRECT calls from one wallet: router-routed
/// swaps are exempt, and trades made on the native pool never reach it.
pub fn check_rate_limit(
    deps: &mut DepsMut,
    env: &Env,
    pool_specs: &PoolSpecs,
    sender: &Addr,
) -> Result<(), ContractError> {
    enforce_min_interval(deps, env, pool_specs, sender, &USER_LAST_COMMIT)
}

/// Separate cooldown map (`USER_LAST_LIQUIDITY_OP`) kept distinct from the
/// swap/commit map. No current entry point stamps it.
pub fn check_liquidity_rate_limit(
    deps: &mut DepsMut,
    env: &Env,
    pool_specs: &PoolSpecs,
    sender: &Addr,
) -> Result<(), ContractError> {
    enforce_min_interval(deps, env, pool_specs, sender, &USER_LAST_LIQUIDITY_OP)
}

/// Shared min-interval check used by both rate limiters. Reads the last
/// action time from `last_action`, rejects if inside the cooldown, and
/// otherwise stamps the current block time.
fn enforce_min_interval(
    deps: &mut DepsMut,
    env: &Env,
    pool_specs: &PoolSpecs,
    sender: &Addr,
    last_action: &Map<&Addr, u64>,
) -> Result<(), ContractError> {
    if let Some(last_action_time) = last_action.may_load(deps.storage, sender)? {
        let time_since_last = env.block.time.seconds().saturating_sub(last_action_time);

        if time_since_last < pool_specs.min_commit_interval {
            let wait_time = pool_specs
                .min_commit_interval
                .saturating_sub(time_since_last);
            return Err(ContractError::TooFrequentCommits { wait_time });
        }
    }

    last_action.save(deps.storage, sender, &env.block.time.seconds())?;

    Ok(())
}

pub fn enforce_transaction_deadline(
    current: Timestamp,
    transaction_deadline: Option<Timestamp>,
) -> Result<(), ContractError> {
    if let Some(dl) = transaction_deadline {
        if current > dl {
            return Err(ContractError::TransactionExpired {});
        }
    }
    Ok(())
}

pub fn decimal2decimal256(dec_value: Decimal) -> StdResult<Decimal256> {
    Decimal256::from_atomics(dec_value.atomics(), dec_value.decimal_places()).map_err(|_| {
        StdError::generic_err(format!(
            "Failed to convert Decimal {} to Decimal256",
            dec_value
        ))
    })
}

pub fn get_bank_transfer_to_msg(
    recipient: &Addr,
    denom: &str,
    amount: Uint128,
) -> StdResult<CosmosMsg> {
    let transfer_bank_msg = BankMsg::Send {
        to_address: recipient.into(),
        amount: vec![Coin {
            denom: denom.to_string(),
            amount,
        }],
    };
    let transfer_bank_cosmos_msg: CosmosMsg = transfer_bank_msg.into();
    Ok(transfer_bank_cosmos_msg)
}

/// Canonical "unknown reply id" message format for consuming
/// contracts' `reply()` entry points. Including
/// the `pool_kind` discriminator in the message makes off-chain log
/// scrapers able to filter by pool kind without parsing the
/// surrounding event context. Use [`crate::state::POOL_KIND_COMMIT`]
/// (or [`crate::state::POOL_KIND_COMMIT`]) for `pool_kind`.
pub fn unknown_reply_id_msg(pool_kind: &str, reply_id: u64) -> String {
    format!("{}-pool reply: unknown reply id {}", pool_kind, reply_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::Timestamp;

    #[test]
    fn decimal2decimal256_preserves_value() {
        let d = Decimal::percent(37);
        let d256 = decimal2decimal256(d).unwrap();
        // Round-trip through string: the "0.37" output should match.
        assert_eq!(d.to_string(), d256.to_string());
    }

    #[test]
    fn decimal2decimal256_zero() {
        let d256 = decimal2decimal256(Decimal::zero()).unwrap();
        assert_eq!(d256.to_string(), "0");
    }

    #[test]
    fn decimal2decimal256_one() {
        let d256 = decimal2decimal256(Decimal::one()).unwrap();
        assert_eq!(d256.to_string(), "1");
    }

    #[test]
    fn enforce_transaction_deadline_no_deadline_always_ok() {
        let current = Timestamp::from_seconds(1_000_000);
        assert!(enforce_transaction_deadline(current, None).is_ok());
    }

    #[test]
    fn enforce_transaction_deadline_future_is_ok() {
        let current = Timestamp::from_seconds(1_000_000);
        let future = Timestamp::from_seconds(1_000_100);
        assert!(enforce_transaction_deadline(current, Some(future)).is_ok());
    }

    #[test]
    fn enforce_transaction_deadline_equal_is_ok() {
        // Equal is NOT expired: the check is `current > dl`.
        let current = Timestamp::from_seconds(1_000_000);
        assert!(enforce_transaction_deadline(current, Some(current)).is_ok());
    }

    #[test]
    fn enforce_transaction_deadline_past_rejects() {
        let current = Timestamp::from_seconds(1_000_000);
        let past = Timestamp::from_seconds(999_999);
        let r = enforce_transaction_deadline(current, Some(past));
        assert!(matches!(r, Err(ContractError::TransactionExpired {})));
    }

    #[test]
    fn swap_and_liquidity_rate_limits_use_independent_maps() {
        // Swaps/commits (`USER_LAST_COMMIT`) and liquidity ops
        // (`USER_LAST_LIQUIDITY_OP`) key *separate* cooldown maps, so a
        // swap — whose CW20 path can be made to stamp an arbitrary
        // address — can never block a withdrawal. This pins that
        // independence.
        use crate::state::PoolSpecs;
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::{Addr, Decimal};

        let mut deps = mock_dependencies();
        let env = mock_env();
        let specs = PoolSpecs {
            lp_fee: Decimal::permille(3),
            min_commit_interval: 13,
        };
        let user = Addr::unchecked("user");

        // A swap stamps the swap/commit cooldown at the current block time.
        check_rate_limit(&mut deps.as_mut(), &env, &specs, &user).unwrap();
        // A second swap in the same block is correctly rate-limited...
        assert!(matches!(
            check_rate_limit(&mut deps.as_mut(), &env, &specs, &user),
            Err(ContractError::TooFrequentCommits { .. })
        ));
        // ...but a liquidity op in the SAME block is NOT blocked: it reads
        // the separate liquidity map, which the swap never touched. A
        // single shared map would have tripped here.
        check_liquidity_rate_limit(&mut deps.as_mut(), &env, &specs, &user).unwrap();
        // The liquidity map enforces its own cooldown independently.
        assert!(matches!(
            check_liquidity_rate_limit(&mut deps.as_mut(), &env, &specs, &user),
            Err(ContractError::TooFrequentCommits { .. })
        ));
    }
}
