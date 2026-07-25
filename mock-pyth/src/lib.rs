//! TEST-ONLY mock of the Pyth CW price-feed contract.
//!
//! Answers `PriceFeed { id }` with a `PriceFeedResponse` whose wire shape
//! matches `factory::pyth_types` — including Pyth's asymmetric JSON
//! encoding (price/conf as strings, expo/publish_time as numbers). Used by
//! the osmosis-test-tube harness to drive the factory's Pyth valuation.
//!
//! `SetPrice` stamps `publish_time = env.block.time` so, after the harness
//! advances the chain past the factory's staleness window, the same feed
//! reads STALE — letting the E2E exercise the fail-closed path. `SetPriceAt`
//! sets an explicit publish_time for the future-skew / exact-age cases.
//!
//! NO access control — anyone may set any price. Never deploy to prod.

use cosmwasm_schema::cw_serde;
#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Env, Int64, MessageInfo, Response, StdError, StdResult,
    Uint64,
};
use cw_storage_plus::Map;

#[cw_serde]
pub struct InstantiateMsg {}

#[cw_serde]
pub enum ExecuteMsg {
    /// Store `(price, expo, conf)` for `price_id`, stamping publish_time to
    /// the current block time.
    SetPrice {
        price_id: String,
        price: i64,
        expo: i32,
        conf: u64,
    },
    /// Store with an EXPLICIT publish_time (for staleness / future-skew tests).
    SetPriceAt {
        price_id: String,
        price: i64,
        expo: i32,
        conf: u64,
        publish_time: i64,
    },
}

#[cw_serde]
pub enum QueryMsg {
    PriceFeed { id: String },
}

// ---- Pyth wire shapes (must match factory::pyth_types exactly) ----
#[cw_serde]
pub struct PythPriceRetrievalResponse {
    pub price: Int64,
    pub conf: Uint64,
    pub expo: i32,
    pub publish_time: i64,
}
#[cw_serde]
pub struct PriceFeed {
    pub id: String,
    pub price: PythPriceRetrievalResponse,
    pub ema_price: PythPriceRetrievalResponse,
}
#[cw_serde]
pub struct PriceFeedResponse {
    pub price_feed: Option<PriceFeed>,
    pub price: Option<PythPriceRetrievalResponse>,
}

#[cw_serde]
pub struct Stored {
    pub price: i64,
    pub expo: i32,
    pub conf: u64,
    pub publish_time: i64,
}

pub const PRICES: Map<&str, Stored> = Map::new("prices");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> StdResult<Response> {
    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::SetPrice {
            price_id,
            price,
            expo,
            conf,
        } => {
            let stored = Stored {
                price,
                expo,
                conf,
                publish_time: env.block.time.seconds() as i64,
            };
            PRICES.save(deps.storage, &price_id, &stored)?;
            Ok(Response::new().add_attribute("action", "set_price"))
        }
        ExecuteMsg::SetPriceAt {
            price_id,
            price,
            expo,
            conf,
            publish_time,
        } => {
            let stored = Stored {
                price,
                expo,
                conf,
                publish_time,
            };
            PRICES.save(deps.storage, &price_id, &stored)?;
            Ok(Response::new().add_attribute("action", "set_price_at"))
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::PriceFeed { id } => {
            let s = PRICES
                .may_load(deps.storage, &id)?
                .ok_or_else(|| StdError::generic_err("price feed not found"))?;
            let retr = PythPriceRetrievalResponse {
                price: Int64::new(s.price),
                conf: Uint64::new(s.conf),
                expo: s.expo,
                publish_time: s.publish_time,
            };
            to_json_binary(&PriceFeedResponse {
                price_feed: Some(PriceFeed {
                    id,
                    price: retr.clone(),
                    ema_price: retr,
                }),
                price: None,
            })
        }
    }
}
