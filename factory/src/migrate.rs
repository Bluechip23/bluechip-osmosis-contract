#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{DepsMut, Empty, Env, Response, StdError};
use cw2::{get_contract_version, set_contract_version};
use semver::Version;

use crate::error::ContractError;
use crate::{CONTRACT_NAME, CONTRACT_VERSION};

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, _msg: Empty) -> Result<Response, ContractError> {
    let stored_version = get_contract_version(deps.storage)?;

    // M-04 — refuse to migrate onto a DIFFERENT contract's storage. Without
    // this cw2 contract-name check, migrating this code id over another
    // contract instance whose stored version merely parses as an
    // `<=`-current semver would pass the downgrade gate below, overwrite the
    // cw2 name, and reinterpret foreign storage as factory state (the
    // registry back-fill would then walk arbitrary bytes). Fail closed.
    if stored_version.contract != CONTRACT_NAME {
        return Err(ContractError::Std(StdError::generic_err(format!(
            "migrate: contract name mismatch — stored \"{}\", expected \"{}\"; \
             refusing to migrate onto foreign storage",
            stored_version.contract, CONTRACT_NAME
        ))));
    }

    let current: Version = CONTRACT_VERSION.parse()?;
    let stored_semver: Version = stored_version.version.parse()?;

    // Strictly reject downgrades. The chain has already replaced the wasm
    // bytecode by the time this handler runs — a no-op here just leaves
    // the cw2 version stale while running the older code. A hard `Err`
    // causes the chain to revert the migration and keep the previous
    // (newer) wasm in place.
    //
    // Equal-version migrations are allowed for idempotent re-runs;
    // strictly-greater stored is rejected.
    if stored_semver > current {
        return Err(ContractError::DowngradeRefused {
            stored: stored_semver.to_string(),
            current: current.to_string(),
        });
    }

    // PAIRS back-fill. Older deployments registered pools through the
    // pre-uniqueness `register_pool`, so `PAIRS` is empty even though
    // pools exist. Walk `POOLS_BY_ID` once and insert one entry per
    // pair, keeping the FIRST pool seen for any given pair (lowest
    // `pool_id`) and skipping subsequent duplicates. This preserves
    // any legacy duplicates already registered (they remain queryable
    // via `POOLS_BY_ID` / `POOLS_BY_CONTRACT_ADDRESS`) but blocks any
    // FURTHER duplicate creations of the same pair after migration —
    // which is the security-relevant invariant we care about.
    //
    // `range(..)` already iterates in ascending pool_id order, so the
    // first-seen pool wins naturally without a sort.
    //
    // M-05 — one-time gate. The back-fill exists only to index pools that
    // predate the uniqueness/reverse-index maps. Once it has run (or on any
    // fresh deployment, which sets the flag at instantiate), skip the O(N)
    // registry walk entirely so a growing registry can never make `migrate`
    // exceed the block gas limit and brick the contract's upgradeability.
    // Pools created after this point self-index through `register_pool`.
    let backfill_done = crate::state::REGISTRY_BACKFILL_DONE
        .may_load(deps.storage)?
        .unwrap_or(false);

    let mut backfilled: u32 = 0;
    let mut legacy_duplicates: u32 = 0;
    let mut addr_index_backfilled: u32 = 0;

    if !backfill_done {
        // Idempotent within this single run: if PAIRS is already populated,
        // the `may_load` check below short-circuits each entry as a no-op.
        let pool_ids: Vec<u64> = crate::state::POOLS_BY_ID
            .keys(deps.storage, None, None, cosmwasm_std::Order::Ascending)
            .collect::<cosmwasm_std::StdResult<Vec<u64>>>()?;
        // POOL_ID_BY_ADDRESS reverse-index back-fill. Same walk as PAIRS,
        // no extra IO. Idempotent — `may_load`-then-save short-circuits if
        // already populated by a prior migrate or a fresh register_pool.
        for pool_id in pool_ids {
        let details = crate::state::POOLS_BY_ID.load(deps.storage, pool_id)?;
        let key = crate::state::canonical_pair_key(&details.pool_token_info);
        if crate::state::PAIRS
            .may_load(deps.storage, key.clone())?
            .is_none()
        {
            crate::state::PAIRS.save(deps.storage, key, &pool_id)?;
            backfilled += 1;
        } else {
            legacy_duplicates += 1;
        }
        if crate::state::POOL_ID_BY_ADDRESS
            .may_load(deps.storage, details.creator_pool_addr.clone())?
            .is_none()
        {
            crate::state::POOL_ID_BY_ADDRESS.save(
                deps.storage,
                details.creator_pool_addr.clone(),
                &pool_id,
            )?;
            addr_index_backfilled += 1;
        }
        }
        // Mark the one-time back-fill complete so future migrations skip
        // the registry walk (M-05).
        crate::state::REGISTRY_BACKFILL_DONE.save(deps.storage, &true)?;
    }

    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    Ok(Response::new()
        .add_attribute("action", "migrate")
        .add_attribute("from", stored_version.version)
        .add_attribute("to", CONTRACT_VERSION)
        .add_attribute("pairs_backfilled", backfilled.to_string())
        .add_attribute(
            "legacy_duplicate_pairs_skipped",
            legacy_duplicates.to_string(),
        )
        .add_attribute(
            "pool_id_by_address_backfilled",
            addr_index_backfilled.to_string(),
        ))
}
