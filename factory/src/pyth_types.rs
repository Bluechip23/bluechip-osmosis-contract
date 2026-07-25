//! Pyth on-chain message-shape mirrors (ported from the original
//! `bluechip-contracts` Pyth integration).
//!
//! These mirror the types the upstream Pyth Cosmos contract
//! (`pyth-sdk-cw`) exposes over the wasm query boundary. They are
//! re-implemented here rather than pulling the full Pyth SDK into the
//! factory's dependency graph. If the on-chain Pyth contract bumps its
//! schema, revalidate these against the new wire format — the read in
//! `usd_price::probe_pyth_usd_rate` deserializes into `PriceFeedResponse`
//! and will fail at runtime (fail-closed) if any field is missing/renamed.
//!
//! Source of truth: <https://github.com/pyth-network/pyth-crosschain>
//! (last verified against pyth-sdk-cw v1.x).

use cosmwasm_schema::cw_serde;
use cosmwasm_std::{Int64, Uint64};

/// Pyth's on-chain CW contract uses ASYMMETRIC JSON encoding for the
/// integer fields: `price` and `conf` come over as JSON STRINGS
/// (Cosmos-SDK convention for i64/u64 to avoid JS precision loss), while
/// `expo` (i32) and `publish_time` (i64) come over as plain JSON NUMBERS.
/// Mirror that exactly — `Int64`/`Uint64` for the string-encoded fields,
/// raw `i32`/`i64` for the number-encoded ones. Read sites unwrap the
/// wrappers via `.i64()` / `.u64()`.
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

/// Response to `PythQueryMsg::PriceFeed`. The upstream contract returns
/// the price inside `price_feed`; some mock/legacy shapes put it in the
/// bare `price` field, so both are accepted (the read prefers
/// `price_feed` and validates its `id`).
#[cw_serde]
pub struct PriceFeedResponse {
    pub price_feed: Option<PriceFeed>,
    pub price: Option<PythPriceRetrievalResponse>,
}

#[cw_serde]
pub enum PythQueryMsg {
    /// The canonical Pyth CW query: fetch a feed by its 64-hex id.
    PriceFeed { id: String },
    /// Legacy/mock accessor kept for the test double.
    GetPrice { price_id: String },
}
