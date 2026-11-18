//! Shared core for Bluechip pools on Osmosis.
//!
//! This crate is a pure library — it exports no `#[entry_point]`s.
//! Consuming pool contract crates provide their own
//! instantiate/execute/query/migrate/reply entry points and dispatch
//! into the handler functions re-exported here.
//!
//! Scope:
//! - Native-module message builders (TokenFactory, GAMM, poolmanager).
//! - Swap routing through the native pool: slippage floor, the
//!   one-directional liquidity breaker, reply forwarding.
//! - Asset handling for the two bank denoms of a pool (OSMO + the
//!   creator TokenFactory denom).
//! - Shared admin ops: pause, unpause, emergency
//!   withdraw (initiate + execute + cancel), ensure_not_drained.
//! - Shared state items and structs backing the above.
//!
//! Out of scope (lives in the consuming contract crates):
//! - Commit-phase logic: commit, threshold crossing, distribution,
//!   claim-creator-excess, claim-creator-fees, retry-factory-notify,
//!   factory-backed USD conversions.  (creator-pool/)
//! - Entry points, factory message dispatch, contract-level tests.
//!
//! Intended consumers:
//! - `creator-pool` — the two-phase pool. Extends this crate with
//!   commit-phase state and handlers.

pub mod admin;
pub mod asset;
pub mod error;
pub mod generic;
pub mod msg;
/// Osmosis-native message builders (TokenFactory / GAMM / poolmanager).
pub mod osmosis_msgs;
pub mod query;
pub mod state;
pub mod swap;
