// ---------------------------------------------------------------------------
// Storage-key CHANGELOG
//
// Most `Item` / `Map` constants in this file use a key string that matches
// the Rust identifier (e.g. `POOLS_BY_ID -> "pools_by_id"`). The drifts
// below are kept on purpose for migration compatibility — renaming the
// key would orphan existing chain state. Add a row here when introducing
// a new drift, or removing one (which is a breaking migration in itself).
//
// Const                          Storage key                     Reason
// ------------------------------ ------------------------------- -------------------------------------------------
// POOLS_BY_CONTRACT_ADDRESS      "pools_by_contract_address"     Matches.
//
// Unlisted Items/Maps follow the convention "key == lowercase(IDENT)";
// any future addition that diverges should be appended here.
// ---------------------------------------------------------------------------

use crate::asset::TokenType;
use crate::pool_struct::{PoolDetails, ThresholdPayoutAmounts};
use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Addr, Binary, Coin, Decimal, StdResult, Storage, Timestamp, Uint128};
use cw_storage_plus::{Item, Map};
use pool_factory_interfaces::PoolStateResponseForFactory;

pub const FACTORYINSTANTIATEINFO: Item<FactoryInstantiate> = Item::new("config");
// In-flight pool creations keep no storage state: the creation context
// (`pool_struct::TempPoolCreation`) rides the SubMsg payload through the
// reply chain, and on any failure the whole tx reverts (every step uses
// `SubMsg::reply_on_success`), so a create leaves no trace until the
// finalize step writes the pool registry.
pub const PENDING_CONFIG: Item<PendingConfig> = Item::new("pending_config");
pub const POOL_COUNTER: Item<u64> = Item::new("pool_counter");

/// M-05 — one-time gate for the legacy registry back-fill in `migrate`.
/// Fresh deployments set this `true` at instantiate (they maintain PAIRS /
/// POOL_ID_BY_ADDRESS through `register_pool` from day one, so no back-fill
/// is ever needed), which makes `migrate` skip the O(N) registry walk
/// entirely. A genuinely-legacy contract upgraded from pre-index code has
/// this unset, so its FIRST `migrate` runs the back-fill once and then sets
/// the flag; every subsequent `migrate` skips it. This removes the
/// "unbounded walk re-run on every migration" upgrade-liveness hazard.
pub const REGISTRY_BACKFILL_DONE: Item<bool> = Item::new("registry_backfill_done");

// Three coupled pool-registry maps. They MUST stay in sync — every pool
// that exists must appear in all three. Always go through `register_pool`
// rather than touching them individually.
// - POOLS_BY_ID:               pool_id  -> PoolDetails (token info, addresses)
// - POOLS_BY_CONTRACT_ADDRESS: pool addr -> snapshot used by queries
// - PAIRS:                     canonical (asset_a, asset_b) key -> pool_id.
// Single-pool-per-pair guard. The Uniswap-style invariant: at most one
// pool exists per (asset_a, asset_b) tuple. Without it, any sender can
// register an arbitrary number of identical pairs (each from a different
// `info.sender` to bypass the per-address rate limit), bloating the
// registry and fragmenting LP.
pub const POOLS_BY_ID: Map<u64, PoolDetails> = Map::new("pools_by_id");
pub const POOLS_BY_CONTRACT_ADDRESS: Map<Addr, PoolStateResponseForFactory> =
    Map::new("pools_by_contract_address");
pub const PAIRS: Map<(String, String), u64> = Map::new("pairs");

/// Reverse index: pool contract address -> `pool_id`. Maintained alongside
/// `POOLS_BY_ID` by `register_pool` so any caller that has a pool address
/// and needs the full `PoolDetails` can do two O(1) loads
/// (`POOL_ID_BY_ADDRESS.load(addr) -> POOLS_BY_ID.load(id)`) instead of
/// an O(N) linear scan of `POOLS_BY_ID`.
///
/// MUST stay in sync with `POOLS_BY_ID`. `register_pool` writes both
/// atomically. Direct writes outside `register_pool` risk drift.
pub const POOL_ID_BY_ADDRESS: Map<Addr, u64> = Map::new("pool_id_by_address");

// Standard timelock applied to admin-initiated mutations of factory state
// (config, pool config, pool upgrades). 48h gives the
// community a full two days to observe a pending change and respond.
// Single source of truth — every propose/execute pair below MUST use this
// constant rather than spelling out `86400 * 2`.
#[cfg(not(feature = "integration_short_timing"))]
pub const ADMIN_TIMELOCK_SECONDS: u64 = 86_400 * 2;
// `--features integration_short_timing` shortens to 120s for local integration tests.
#[cfg(feature = "integration_short_timing")]
pub const ADMIN_TIMELOCK_SECONDS: u64 = 120;
pub const PENDING_POOL_UPGRADE: Item<PoolUpgrade> = Item::new("pending_upgrade");

/// Per-pool flag set the first (and only) time the pool's
/// `NotifyThresholdCrossed` callback is accepted. Idempotency gate — a
/// retried notify after the first success is rejected rather than
/// re-recording the crossing.
pub const POOL_THRESHOLD_CROSSED: Map<u64, bool> = Map::new("pool_threshold_crossed");
pub const PENDING_POOL_CONFIG: Map<u64, PendingPoolConfig> = Map::new("pending_pool_config");

// Per-address rate limit on commit-pool creation: timestamp of each
// creator's last successful `Create`. Defends against spam that would
// bloat the registry and gas-amplify any future per-pool storage scan.
// Per-address (not global) so coordinated multi-address spam still has
// to fund + sign from each address it rotates through.
pub const LAST_COMMIT_POOL_CREATE_AT: Map<Addr, Timestamp> = Map::new("last_commit_pool_create_at");

/// Time-ordered secondary index over `LAST_COMMIT_POOL_CREATE_AT`,
/// keyed by `(timestamp_secs, Addr)`. Exists so the permissionless
/// `PruneRateLimits` handler can iterate stale entries in O(stale_count)
/// instead of walking the full address-keyed map (which is alphabetic
/// in `Addr` and therefore uncorrelated with timestamp — a prune call
/// against a million-entry map would otherwise visit every entry
/// looking for the first stale one).
///
/// Maintained alongside `LAST_COMMIT_POOL_CREATE_AT` by the create
/// handler: on each stamp it removes the prior `(old_ts, addr)`
/// entry (if any) and inserts the new `(now_ts, addr)`. Prune deletes
/// from BOTH on each stale entry it processes. Both updates ride in
/// the same tx as the primary save, so a failure reverts both maps
/// atomically and they cannot drift.
pub const COMMIT_POOL_CREATE_TS_INDEX: Map<(u64, Addr), ()> = Map::new("commit_pool_create_ts_idx");

/// Minimum seconds between consecutive `Create` calls from the same
/// `info.sender`. 3600s = 1h. Reasonable for legitimate creator-pool
/// flows (you launch one token at a time) and asymmetric enough against
/// spam that even a fully-funded attacker would need to rotate through
/// thousands of addresses to materially bloat the registry.
#[cfg(not(feature = "integration_short_timing"))]
pub const COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS: u64 = 3600;
#[cfg(feature = "integration_short_timing")]
pub const COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS: u64 = 30;

#[cw_serde]
pub struct PendingPoolConfig {
    pub pool_id: u64,
    pub update: crate::pool_struct::PoolConfigUpdate,
    pub effective_after: Timestamp,
}

#[cw_serde]
pub struct FactoryInstantiate {
    pub factory_admin_address: Addr,
    /// Commit threshold each creator pool must raise before it seeds its
    /// AMM and opens for swaps. USD-denominated, 6 decimals
    /// (`25_000_000_000` = $25,000). Commits are made in `bluechip_denom`
    /// and valued against this target via the chain's x/twap price of the
    /// configured `pricing_pool_id` (see `crate::usd_price`).
    pub commit_threshold_limit_usd: Uint128,
    pub cw20_token_contract_id: u64,
    pub cw721_nft_contract_id: u64,
    pub create_pool_wasm_contract_id: u64,
    pub bluechip_wallet_address: Addr,
    pub commit_fee_bluechip: Decimal,
    pub commit_fee_creator: Decimal,
    pub max_bluechip_lock_per_pool: Uint128,
    pub creator_excess_liquidity_lock_days: u64,
    /// Canonical native bank denom pools pair against on this chain —
    /// the chain's main asset (e.g. `"uatom"` on Cosmos Hub, `"untrn"`
    /// on Neutron). Pinned at factory instantiate time and enforced
    /// whenever a pool is created: the `TokenType::Native { denom }` entry
    /// in `pool_token_info` MUST match this value exactly. Prevents an
    /// attacker from registering a pool with an arbitrary native denom
    /// (tokenfactory-minted fake, low-value IBC denom, etc.) and
    /// having every downstream commit path treat that denom's
    /// balance as the real pairing asset.
    pub bluechip_denom: String,
    /// Osmosis pool id whose arithmetic TWAP prices `bluechip_denom`
    /// against `usd_quote_denom`. Point this at the chain's deepest
    /// native/USD-stable pool (e.g. the main OSMO/USDC pool) — the
    /// manipulation cost of every USD valuation in the protocol is the
    /// cost of moving THIS pool for `twap_window_seconds`.
    pub pricing_pool_id: u64,
    /// The USD-stable quote denom on the pricing pool (e.g. Noble USDC's
    /// IBC denom on Osmosis). Must be a 6-decimal dollar asset — the
    /// TWAP quote-per-base price is consumed directly as USD-per-native.
    pub usd_quote_denom: String,
    /// Arithmetic-TWAP lookback window in seconds. Bounds:
    /// [`crate::usd_price::TWAP_WINDOW_MIN_SECONDS`],
    /// [`crate::usd_price::TWAP_WINDOW_MAX_SECONDS`]. Default 600 (10min).
    #[serde(default = "default_twap_window_seconds")]
    pub twap_window_seconds: u64,
    /// Flat fee charged on every `Create`
    /// call, denominated in base units of `bluechip_denom`.
    /// Forwarded to `bluechip_wallet_address`; surplus refunded to the
    /// caller.
    ///
    /// Tunable via the existing 48h `ProposeConfigUpdate` flow.
    /// Setting this to zero disables the fee entirely (legitimate
    /// configuration choice for permissioned deployments).
    pub pool_creation_fee: Uint128,
    /// GAMM pool-creation fee that the chain's `x/gamm` module auto-charges
    /// when `MsgCreateBalancerPool` executes at threshold crossing. The
    /// pool contract must hold this coin at that moment, so the factory
    /// collects it from the creator at `Create` time (IN ADDITION to the
    /// flat `pool_creation_fee` above) and forwards it into the pool's
    /// instantiate `funds`. The pool holds it until threshold crossing.
    /// Zero amount disables collection (e.g. test environments where the
    /// gamm create fee is waived).
    ///
    /// `#[serde(default)]` lets pre-this-field factory records deserialize
    /// with an empty (zero) coin.
    #[serde(default = "default_gamm_pool_creation_fee")]
    pub gamm_pool_creation_fee: Coin,
    /// Per-pool threshold-payout splits applied when a commit pool
    /// crosses its threshold. The sum is also used as the CW20
    /// mint cap pinned at create time, so changing these values
    /// after launch only affects pools created AFTER the timelock
    /// expires — already-instantiated pools have their cap baked in.
    ///
    /// `#[serde(default)]` lets pre-this-field factory records
    /// deserialize cleanly with the launch defaults
    /// (creator 325e9 / bluechip 25e9 / pool_seed 350e9 / commit_return 500e9
    /// = 1.2e12 total). New deployments must still pass an explicit value
    /// at instantiate time; the default exists purely for migration
    /// compatibility with old serialized config snapshots.
    #[serde(default)]
    pub threshold_payout_amounts: ThresholdPayoutAmounts,
    /// Timelock between `EmergencyWithdraw` Phase 1 (initiate) and Phase 2
    /// (drain) on every pool spawned by this factory. Queried at runtime by
    /// `pool-core::execute_emergency_withdraw_initiate` via the
    /// `FactoryQueryMsg::EmergencyWithdrawDelaySeconds` cross-contract query,
    /// so pools always read the current factory-side value rather than a
    /// snapshot taken at instantiate time.
    ///
    /// Default `86_400` (24h). Range-validated in `validate_factory_config`:
    /// minimum `EMERGENCY_WITHDRAW_DELAY_MIN_SECONDS` (60s), maximum
    /// `EMERGENCY_WITHDRAW_DELAY_MAX_SECONDS` (7 days).
    ///
    /// Tunable via the standard 48h `ProposeConfigUpdate` flow. Changing
    /// this affects in-flight emergency-withdraws? No — the
    /// `effective_after` timestamp is computed at initiate time from the
    /// then-current value and stored in `PENDING_EMERGENCY_WITHDRAW`; a
    /// later config update changes only the cadence applied to NEXT
    /// initiations.
    ///
    /// `#[serde(default)]` lets old serialized factory records (no field)
    /// deserialize cleanly with the legacy default, so existing
    /// deployments behave identically until the admin proposes an update.
    #[serde(default = "default_emergency_withdraw_delay_seconds")]
    pub emergency_withdraw_delay_seconds: u64,
}

pub const EMERGENCY_WITHDRAW_DELAY_MIN_SECONDS: u64 = 60;
pub const EMERGENCY_WITHDRAW_DELAY_MAX_SECONDS: u64 = 86_400 * 7;

pub fn default_emergency_withdraw_delay_seconds() -> u64 {
    86_400
}

pub fn default_twap_window_seconds() -> u64 {
    600
}

/// Default (zero) GAMM pool-creation fee — collection disabled.
pub fn default_gamm_pool_creation_fee() -> Coin {
    Coin {
        denom: String::new(),
        amount: Uint128::zero(),
    }
}

#[cw_serde]
pub struct PendingConfig {
    pub new_config: FactoryInstantiate,
    pub effective_after: Timestamp,
}

/// Lifecycle stages reported by the `PoolCreationStatus` query's wire
/// shape. Pool creation is atomic within a single tx (the context rides
/// SubMsg payloads and every step is `reply_on_success`), so no
/// intermediate stage is ever externally observable — the enum exists
/// for response-schema compatibility.
#[cw_serde]
pub enum CreationStatus {
    Started,
    TokenCreated,
    NftCreated,
    PoolCreated,
    Completed,
    Failed,
    CleaningUp,
}

#[cw_serde]
pub struct PoolUpgrade {
    pub new_code_id: u64,
    pub migrate_msg: Binary,
    pub pools_to_upgrade: Vec<u64>,
    /// Number of entries from `pools_to_upgrade` for which a first-pass
    /// decision (migrate-or-defer) has been recorded. Once
    /// `upgraded_count == pools_to_upgrade.len()`, the first pass is
    /// complete and subsequent `ContinuePoolUpgrade` calls drain
    /// `pending_retry` instead.
    pub upgraded_count: u32,
    /// Pools that were paused on their first-pass turn (or, on a retry
    /// pass, are still paused / unreachable). Each `ContinuePoolUpgrade`
    /// after the first pass completes takes up to `batch_size` entries
    /// from the front, re-queries `IsPaused`, and migrates the ones
    /// that have unpaused since. Pools still paused stay in the queue;
    /// the admin can `CancelPoolUpgrade` to abandon any tail that
    /// remains permanently paused. `#[serde(default)]` lets pre-this-
    /// field PENDING_POOL_UPGRADE records (there shouldn't be any —
    /// this is launch v1 — but the default is defensive) deserialize
    /// cleanly with an empty retry queue.
    #[serde(default)]
    pub pending_retry: Vec<u64>,
    pub effective_after: Timestamp,
}

// ---------------------------------------------------------------------------
// Pool registry helpers
// ---------------------------------------------------------------------------
// Centralized so the three pool-registry maps cannot drift. Direct writes to
// POOLS_BY_ID / POOLS_BY_CONTRACT_ADDRESS / PAIRS outside this module risk
// leaving the factory's view of pools internally inconsistent.

/// Canonicalized fingerprint of a single side of a pool pair.
///
/// Native denoms and CW20 contract addresses are both stringly-typed, so a
/// kind-tag prefix is required to keep them in disjoint namespaces — a
/// chain that ever ended up with a CW20 contract address that happens to
/// equal a native denom string would otherwise alias two different
/// asset references onto the same key. The prefixes (`n:` for native,
/// `c:` for creator-token) are short, opaque to user-facing surfaces
/// (the key is internal-only), and stable forever — changing them is a
/// breaking storage migration.
fn token_fingerprint(t: &TokenType) -> String {
    match t {
        TokenType::Native { denom } => format!("n:{}", denom),
        // The creator token is a TokenFactory bank denom now; still tagged
        // `c:` so it stays in a disjoint namespace from plain natives.
        TokenType::CreatorToken { denom } => format!("c:{}", denom),
    }
}

/// Order-independent key for the `(asset_a, asset_b)` uniqueness map.
///
/// The two fingerprints are sorted lexicographically before being returned
/// as `(min, max)`, so `[A, B]` and `[B, A]` map to the same storage slot.
/// This matches Uniswap V2's `getPair[a][b] == getPair[b][a]` convention
/// and is the right shape for "at most one pool per unordered pair." If a
/// future pool variant ever needs to permit parallel pools at different
/// fee tiers / curve types / hook configurations, widen this key with the
/// extra discriminator(s) — do NOT add a parallel uniqueness map.
pub fn canonical_pair_key(pair: &[TokenType; 2]) -> (String, String) {
    let a = token_fingerprint(&pair[0]);
    let b = token_fingerprint(&pair[1]);
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Atomically register a freshly created pool across all three registry
/// maps. Rejects with a generic_err if `pair` already exists in `PAIRS`
/// — this is the canonical guard against silent duplicate registrations
/// from any code path (entry-point pre-check, future admin restore,
/// migrate back-fill, etc). The pre-check at the create entry points
/// exists purely to fail-fast before the caller's fee is forwarded;
/// THIS is the load-bearing check.
///
/// Initial `PoolStateResponseForFactory` is materialized from `pool_details`
/// — caller doesn't need to construct it. Reserves and TWAP accumulators
/// start at zero; the pool itself updates them as activity flows through.
pub fn register_pool(
    storage: &mut dyn Storage,
    pool_id: u64,
    pool_address: &Addr,
    pool_details: &PoolDetails,
) -> StdResult<()> {
    let pair_key = canonical_pair_key(&pool_details.pool_token_info);
    if let Some(existing) = PAIRS.may_load(storage, pair_key.clone())? {
        return Err(cosmwasm_std::StdError::generic_err(format!(
            "duplicate pair: pool_id {} already registered for ({}, {})",
            existing, pair_key.0, pair_key.1
        )));
    }
    PAIRS.save(storage, pair_key, &pool_id)?;

    POOLS_BY_ID.save(storage, pool_id, pool_details)?;
    // Reverse index — see `POOL_ID_BY_ADDRESS` doc. Written here so the
    // three-map invariant becomes a four-map invariant inside this
    // single helper rather than every call site having to know about it.
    POOL_ID_BY_ADDRESS.save(storage, pool_address.clone(), &pool_id)?;

    let asset_strings: Vec<String> = pool_details
        .pool_token_info
        .iter()
        .map(|t| match t {
            TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom.clone(),
        })
        .collect();

    POOLS_BY_CONTRACT_ADDRESS.save(
        storage,
        pool_address.clone(),
        &PoolStateResponseForFactory {
            pool_contract_address: pool_address.clone(),
            nft_ownership_accepted: false,
            reserve0: Uint128::zero(),
            reserve1: Uint128::zero(),
            total_liquidity: Uint128::zero(),
            block_time_last: 0,
            price0_cumulative_last: Uint128::zero(),
            price1_cumulative_last: Uint128::zero(),
            assets: asset_strings,
        },
    )?;

    Ok(())
}

/// Resolve a pool *contract address* against the registry via the
/// `POOL_ID_BY_ADDRESS` reverse index. Returns `None` when the address is
/// not a registered pool.
///
/// In production a miss in `POOL_ID_BY_ADDRESS` combined with a hit in
/// `POOLS_BY_CONTRACT_ADDRESS` means a write bypassed `register_pool` —
/// a real bug — and is surfaced loudly. In tests, fixtures may
/// write `POOLS_BY_ID` directly, so a linear-scan fallback keeps them
/// resolving without rewrites.
pub(crate) fn lookup_pool_by_addr(
    deps: cosmwasm_std::Deps,
    pool_addr: &Addr,
) -> StdResult<Option<PoolDetails>> {
    if let Some(pool_id) = POOL_ID_BY_ADDRESS.may_load(deps.storage, pool_addr.clone())? {
        return Ok(Some(POOLS_BY_ID.load(deps.storage, pool_id)?));
    }
    #[cfg(not(test))]
    {
        if POOLS_BY_CONTRACT_ADDRESS.has(deps.storage, pool_addr.clone()) {
            return Err(cosmwasm_std::StdError::generic_err(format!(
                "Registry inconsistency: pool {} exists in POOLS_BY_CONTRACT_ADDRESS \
                 but not in POOL_ID_BY_ADDRESS reverse index. Every pool created via \
                 the reply chain populates both atomically via state::register_pool; \
                 reaching this branch means a write bypassed that helper. Investigate \
                 before retrying.",
                pool_addr
            )));
        }
        Ok(None)
    }
    #[cfg(test)]
    {
        use cosmwasm_std::Order;
        for entry in POOLS_BY_ID.range(deps.storage, None, None, Order::Ascending) {
            let (_, details) = entry?;
            if details.creator_pool_addr == *pool_addr {
                return Ok(Some(details));
            }
        }
        Ok(None)
    }
}
