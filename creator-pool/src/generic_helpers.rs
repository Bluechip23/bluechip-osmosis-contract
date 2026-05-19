//! Re-export shim for shared primitives + the genuinely-generic
//! per-commit `update_commit_info`.
//!
//! Shared primitives (`check_rate_limit`, `enforce_transaction_deadline`,
//! `update_pool_fee_growth`, `decimal2decimal256`,
//! `get_bank_transfer_to_msg`) live in `pool_core::generic` and are
//! re-exported below so every existing `use crate::generic_helpers::X;`
//! import resolves unchanged.
//!
//! Threshold-payout orchestration was hoisted to
//! [`crate::commit::threshold_payout`]; the post-threshold batch
//! processor was hoisted to [`crate::commit::distribution_batch`].
//! Re-exports here keep both reachable through the original path so
//! existing call sites (the threshold-crossing handler, the
//! distribution dispatcher) compile unchanged.

pub use pool_core::generic::*;

pub use crate::commit::distribution_batch::{
    calculate_effective_batch_size, process_distribution_batch,
};
pub use crate::commit::threshold_payout::{
    mint_tokens, trigger_threshold_payout, validate_pool_threshold_payments, ThresholdPayoutMsgs,
};

use crate::error::ContractError;
use crate::state::{Committing, COMMIT_INFO};
use cosmwasm_std::{Addr, Deps, Storage, Timestamp, Uint128};

// `with_reentrancy_guard` moved to `pool_core::generic` and reaches
// existing callers in this crate via the `pub use pool_core::generic::*;`
// re-export above. The swap path in `pool_core::swap` and any future
// liquidity / admin caller in either pool crate now share a single
// implementation rather than each open-coding the load/check/save dance.

/// Resolve the bluechip protocol-wallet recipient by live-querying the
/// factory, falling back to the pool-side snapshot if the query fails.
///
/// `COMMITFEEINFO.bluechip_wallet_address` is pinned at pool instantiate
/// from the factory's then-current config. The factory's address is
/// admin-tunable via the standard 48h `ProposeConfigUpdate` flow; a
/// snapshot would leave every pool sending fees and the threshold-cross
/// bluechip-reward to whichever wallet the admin had configured when the
/// pool was created. After a key-compromise-driven rotation that's
/// exactly the wallet we don't want to be sending to.
///
/// Mirrors the pattern already in use on the emergency-drain path
/// (`pool-core::admin::execute_emergency_withdraw_core_drain`,
/// `execute_sweep_unclaimed_emergency_shares`). Fail-soft on query
/// failure (factory paused, migrated, unreachable) so a hostile factory
/// state can't strand commits.
pub fn resolve_live_bluechip_wallet(
    deps: Deps,
    factory_addr: &Addr,
    snapshot_fallback: &Addr,
) -> Addr {
    match deps.querier.query_wasm_smart::<
        pool_factory_interfaces::BluechipWalletResponse,
    >(
        factory_addr.to_string(),
        &pool_factory_interfaces::FactoryQueryMsg::BluechipWalletAddress {},
    ) {
        Ok(resp) => resp.address,
        Err(_) => snapshot_fallback.clone(),
    }
}

pub fn update_commit_info(
    storage: &mut dyn Storage,
    sender: &Addr,
    pool_contract_address: &Addr,
    bluechip_amount: Uint128,
    usd_amount: Uint128,
    timestamp: Timestamp,
) -> Result<(), ContractError> {
    COMMIT_INFO.update(
        storage,
        sender,
        |maybe_committing| -> Result<_, ContractError> {
            match maybe_committing {
                Some(mut committing) => {
                    committing.total_paid_bluechip = committing
                        .total_paid_bluechip
                        .checked_add(bluechip_amount)?;
                    committing.total_paid_usd =
                        committing.total_paid_usd.checked_add(usd_amount)?;
                    committing.last_payment_bluechip = bluechip_amount;
                    committing.last_payment_usd = usd_amount;
                    committing.last_committed = timestamp;
                    Ok(committing)
                }
                // First-commit for this sender: clone only here, where the
                // owned Addr is actually stored. Repeat committers (the
                // common path) pass through zero Addr allocations.
                None => Ok(Committing {
                    pool_contract_address: pool_contract_address.clone(),
                    committer: sender.clone(),
                    total_paid_bluechip: bluechip_amount,
                    total_paid_usd: usd_amount,
                    last_committed: timestamp,
                    last_payment_bluechip: bluechip_amount,
                    last_payment_usd: usd_amount,
                }),
            }
        },
    )?;
    Ok(())
}
