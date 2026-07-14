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

use cosmwasm_std::{Addr, Coin, CosmosMsg, Decimal, Uint128};
use osmosis_std::types::cosmos::base::v1beta1::Coin as OsmoCoin;
use osmosis_std::types::osmosis::gamm::poolmodels::balancer::v1beta1::MsgCreateBalancerPool;
use osmosis_std::types::osmosis::gamm::v1beta1::{PoolAsset, PoolParams};
use osmosis_std::types::osmosis::poolmanager::v1beta1::{MsgSwapExactAmountIn, SwapAmountInRoute};
use osmosis_std::types::osmosis::tokenfactory::v1beta1::{MsgBurn, MsgCreateDenom, MsgMint};

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
/// below `token_out_min_amount` (the slippage floor derived from the
/// caller's belief-price / max-spread). `sender` (the pool contract)
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
