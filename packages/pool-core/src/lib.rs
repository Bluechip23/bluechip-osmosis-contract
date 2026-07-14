//! Shared AMM + liquidity-position core for Bluechip pools.
//!
//! This crate is a pure library — it exports no `#[entry_point]`s.
//! Consuming pool contract crates provide their own
//! instantiate/execute/query/migrate/reply entry points and dispatch
//! into the handler functions re-exported here.
//!
//! Scope:
//! - AMM math: constant-product swap, spread/slippage, price
//! accumulator.
//! - Liquidity positions: deposit, add, remove (partial / full /
//! percentage), collect fees, NFT ownership sync, fee-size
//! multiplier clipping.
//! - Asset handling: pair-shape-agnostic transfer/collect helpers for
//! Native/CW20/CW20-CW20/Native-Native pools.
//! - Shared admin ops: pause, unpause, emergency
//! withdraw (initiate + execute + cancel), ensure_not_drained.
//! - Shared state items and structs backing the above.
//!
//! Out of scope (lives in the consuming contract crates):
//! - Commit-phase logic: commit, threshold crossing, distribution,
//! claim-creator-excess, claim-creator-fees, retry-factory-notify,
//! factory-backed USD conversions.  (creator-pool/)
//! - Entry points, factory message dispatch, contract-level tests.
//!
//! Intended consumers:
//! - `creator-pool` — the two-phase pool. Extends this crate with
//! commit-phase state and handlers.

pub mod admin;
pub mod asset;
pub mod error;
pub mod generic;
pub mod msg;
/// Osmosis-native message builders (TokenFactory / GAMM / poolmanager)
/// backing the migration off the internal CW20 AMM.
pub mod osmosis_msgs;
pub mod query;
pub mod state;
pub mod swap;
