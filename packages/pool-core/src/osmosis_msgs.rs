//! Osmosis-native message builders.
//!
//! Single typed surface for every osmosis-std construction the pool needs
//! as it moves off the internal CW20 AMM and onto chain-native modules:
//!
//! - **TokenFactory** — the creator token becomes a native bank denom
//!   (`factory/{admin}/{subdenom}`) instead of a CW20 contract. The pool
//!   contract is the denom admin, so it can mint at threshold-crossing and
//!   distribution.
//! - **GAMM** — at threshold-crossing the pool seeds a native balancer
//!   pool. Equal weights give the constant-product (`x*y=k`) curve the
//!   retired internal AMM used, so behavior is preserved.
//! - **poolmanager** — post-threshold commits route their swap leg through
//!   the native pool via `MsgSwapExactAmountIn`.
//!
//! Keeping all `.into()`-to-`CosmosMsg` conversions here means the rest of
//! the crate never touches osmosis-std types directly.

use cosmwasm_std::{Addr, Coin, CosmosMsg, CustomQuery, Decimal, QuerierWrapper, Uint128};
use osmosis_std::types::cosmos::base::v1beta1::Coin as OsmoCoin;
use osmosis_std::types::osmosis::gamm::poolmodels::balancer::v1beta1::MsgCreateBalancerPool;
use osmosis_std::types::osmosis::gamm::v1beta1::{PoolAsset, PoolParams};
use osmosis_std::types::osmosis::poolmanager::v1beta1::{
    MsgSwapExactAmountIn, PoolmanagerQuerier, SwapAmountInRoute,
};
use osmosis_std::types::osmosis::tokenfactory::v1beta1::{
    MsgBurn, MsgCreateDenom, MsgMint, MsgSetDenomMetadata,
};
use std::str::FromStr;

/// Balancer pool-asset weight used for BOTH sides. Any pair of *equal*
/// weights produces the 50/50 constant-product curve — identical behavior
/// to the retired internal `x*y=k` AMM. The absolute value is irrelevant
/// as long as both sides match; `1` is the minimal valid weight the gamm
/// module accepts.
const BALANCER_EQUAL_WEIGHT: &str = "1";

/// Convert a cosmwasm bank [`Coin`] into the osmosis-std protobuf coin
/// (amount carried as a decimal string).
fn to_osmo_coin(coin: &Coin) -> OsmoCoin {
    OsmoCoin {
        denom: coin.denom.clone(),
        amount: coin.amount.to_string(),
    }
}

/// The full TokenFactory denom a `subdenom` resolves to when created by
/// `admin`. Deterministic — the pool knows its creator-token denom the
/// moment it knows its own address and chosen subdenom, without waiting
/// for the `MsgCreateDenom` reply.
pub fn full_denom(admin: &Addr, subdenom: &str) -> String {
    format!("factory/{}/{}", admin, subdenom)
}

/// `MsgCreateDenom` — registers `factory/{sender}/{subdenom}`. `sender`
/// becomes the denom admin (mint / burn / change-admin authority).
pub fn create_denom_msg(sender: &Addr, subdenom: &str) -> CosmosMsg {
    MsgCreateDenom {
        sender: sender.to_string(),
        subdenom: subdenom.to_string(),
    }
    .into()
}

/// Register bank Metadata for a TokenFactory `denom` so explorers and
/// wallets render the creator's chosen `name` / `symbol` and the correct
/// decimal scaling instead of the raw `factory/{admin}/{sub}` micro-denom.
/// `sender` must be the denom admin (the pool contract). Emitted right
/// after `MsgCreateDenom` in the same instantiate response — the denom
/// exists by the time this message executes.
///
/// Two denom units are registered: the base micro-denom at exponent 0 and
/// a human `display` unit (the ticker) at `decimals`. When `decimals == 0`
/// only the base unit is registered (a display unit at exponent 0 would
/// collide with the base and be rejected by the bank module).
pub fn set_denom_metadata_msg(
    sender: &Addr,
    denom: &str,
    name: &str,
    symbol: &str,
    decimals: u32,
) -> CosmosMsg {
    use osmosis_std::types::cosmos::bank::v1beta1::{DenomUnit, Metadata};

    let mut denom_units = vec![DenomUnit {
        denom: denom.to_string(),
        exponent: 0,
        aliases: vec![],
    }];
    // The display unit's denom is the ticker (e.g. "MYTOKEN"). It must
    // differ from the base and sit at a higher exponent; skip it entirely
    // for a zero-decimal token so the two units can't collide.
    if decimals > 0 {
        denom_units.push(DenomUnit {
            denom: symbol.to_string(),
            exponent: decimals,
            aliases: vec![],
        });
    }

    MsgSetDenomMetadata {
        sender: sender.to_string(),
        metadata: Some(Metadata {
            description: format!("{} creator token", name),
            denom_units,
            base: denom.to_string(),
            // `display` must name one of the registered denom units. Use
            // the ticker when a display unit exists, else the base denom.
            display: if decimals > 0 {
                symbol.to_string()
            } else {
                denom.to_string()
            },
            name: name.to_string(),
            symbol: symbol.to_string(),
            uri: String::new(),
            uri_hash: String::new(),
        }),
    }
    .into()
}

/// Query the chain's LIVE pool-creation fee (the coins `x/poolmanager`
/// deducts from the sender when `MsgCreateBalancerPool` executes) for a
/// given `denom`.
///
/// Returns `None` if the params query is unavailable on the target chain
/// build or the fee is not denominated in `denom`. Callers treat `None`
/// as "fall back to the factory-configured value." Using the live value
/// at threshold-crossing makes the seed math self-correcting: the pool
/// always reserves exactly what the module will charge, so a governance
/// change to the fee — or a mis-set factory config — can no longer brick
/// the crossing by leaving the pool unable to cover the create fee.
pub fn query_pool_creation_fee<C: CustomQuery>(
    querier: &QuerierWrapper<C>,
    denom: &str,
) -> Option<Uint128> {
    let resp = PoolmanagerQuerier::new(querier).params().ok()?;
    let params = resp.params?;
    params
        .pool_creation_fee
        .iter()
        .find(|c| c.denom == denom)
        .and_then(|c| Uint128::from_str(&c.amount).ok())
}

/// `MsgMint` — mint `amount` of `denom` and credit `mint_to`. `sender`
/// must be the denom admin (the pool contract).
pub fn mint_msg(sender: &Addr, denom: &str, amount: Uint128, mint_to: &Addr) -> CosmosMsg {
    MsgMint {
        sender: sender.to_string(),
        amount: Some(OsmoCoin {
            denom: denom.to_string(),
            amount: amount.to_string(),
        }),
        mint_to_address: mint_to.to_string(),
    }
    .into()
}

/// `MsgBurn` — burn `amount` of `denom` from `burn_from`. `sender` must be
/// the denom admin. Retained for completeness (e.g. reclaiming a failed
/// distribution); not on the hot path.
pub fn burn_msg(sender: &Addr, denom: &str, amount: Uint128, burn_from: &Addr) -> CosmosMsg {
    MsgBurn {
        sender: sender.to_string(),
        amount: Some(OsmoCoin {
            denom: denom.to_string(),
            amount: amount.to_string(),
        }),
        burn_from_address: burn_from.to_string(),
    }
    .into()
}

/// `MsgCreateBalancerPool` seeding a 50/50 (constant-product) pool from
/// the two coins the pool contract holds. `swap_fee` is the LP fee
/// (e.g. `Decimal::permille(3)` for 0.3%); the gamm module wants it as an
/// 18-decimal fixed-point string, which is exactly `Decimal::atomics()`.
/// Exit fee is fixed at zero — nonzero exit fees are deprecated on
/// Osmosis. `future_pool_governor` is left empty (no pool-local gov).
///
/// The `sender` (pool contract) receives the pool's LP shares
/// (`gamm/pool/{id}`) and must already hold both coins in its bank balance
/// plus the chain's pool-creation fee when this message executes.
pub fn create_balancer_pool_msg(
    sender: &Addr,
    coin_a: &Coin,
    coin_b: &Coin,
    swap_fee: Decimal,
) -> CosmosMsg {
    MsgCreateBalancerPool {
        sender: sender.to_string(),
        pool_params: Some(PoolParams {
            swap_fee: swap_fee.atomics().to_string(),
            exit_fee: Decimal::zero().atomics().to_string(),
            smooth_weight_change_params: None,
        }),
        pool_assets: vec![
            PoolAsset {
                token: Some(to_osmo_coin(coin_a)),
                weight: BALANCER_EQUAL_WEIGHT.to_string(),
            },
            PoolAsset {
                token: Some(to_osmo_coin(coin_b)),
                weight: BALANCER_EQUAL_WEIGHT.to_string(),
            },
        ],
        future_pool_governor: String::new(),
    }
    .into()
}

/// `MsgSwapExactAmountIn` — single-hop swap of `token_in` for
/// `token_out_denom` through `pool_id`, reverting if the output would fall
/// below `token_out_min_amount` (the slippage floor — the more protective
/// of an on-chain poolmanager estimate floor and the caller's belief-price
/// floor; see `pool_core::swap::compute_token_out_min`). `sender` (the pool contract)
/// receives the output and forwards it to the committer in the reply.
pub fn swap_exact_amount_in_msg(
    sender: &Addr,
    pool_id: u64,
    token_in: &Coin,
    token_out_denom: &str,
    token_out_min_amount: Uint128,
) -> CosmosMsg {
    MsgSwapExactAmountIn {
        sender: sender.to_string(),
        routes: vec![SwapAmountInRoute {
            pool_id,
            token_out_denom: token_out_denom.to_string(),
        }],
        token_in: Some(to_osmo_coin(token_in)),
        token_out_min_amount: token_out_min_amount.to_string(),
    }
    .into()
}
