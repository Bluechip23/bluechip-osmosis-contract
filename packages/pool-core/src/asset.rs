//! Shared asset-handling helpers. `TokenInfoPoolExt` gives `TokenInfo`
//! the pool-side message-building methods (`into_msg`,
//! `confirm_sent_native_balance`, `deduct_tax`). `PoolPairInfo` is the
//! response type for shared `query_pair_info`.
//!
//! The `pool_factory_interfaces::asset::*` glob re-export keeps
//! `TokenType`, `TokenInfo`, `PoolPairType`, `get_native_denom`, and the
//! various constructors accessible as `pool_core::asset::*` — so any
//! `use pool_core::asset::X;` in downstream crates Just Works.

pub use pool_factory_interfaces::asset::*;

use cosmwasm_std::{
    Addr, BankMsg, Coin, CosmosMsg, MessageInfo, QuerierWrapper, StdError, StdResult,
};
use cw_utils::must_pay;

pub const UBLUECHIP_DENOM: &str = "ubluechip";

// Pool-specific extension methods for TokenInfo.
// These depend on cw20/bank message building which is only needed in the pool contract.
pub trait TokenInfoPoolExt {
    fn deduct_tax(&self, querier: &QuerierWrapper) -> StdResult<Coin>;
    fn into_msg(self, querier: &QuerierWrapper, recipient: Addr) -> StdResult<CosmosMsg>;
    fn confirm_sent_native_balance(&self, message_info: &MessageInfo) -> StdResult<()>;
}

impl TokenInfoPoolExt for TokenInfo {
    fn deduct_tax(&self, _querier: &QuerierWrapper) -> StdResult<Coin> {
        // Both sides are bank denoms now (bluechip Native + the creator
        // TokenFactory denom), so either shape yields a plain bank Coin.
        let amount = self.amount;
        match &self.info {
            TokenType::Native { denom } | TokenType::CreatorToken { denom } => Ok(Coin {
                denom: denom.to_string(),
                amount,
            }),
        }
    }

    fn into_msg(self, querier: &QuerierWrapper, recipient: Addr) -> StdResult<CosmosMsg> {
        // Both the bluechip side and the creator TokenFactory side are
        // native bank coins, so every outgoing transfer is a BankMsg::Send.
        // (Pre-migration the CreatorToken arm built a `Cw20ExecuteMsg::Transfer`.)
        Ok(CosmosMsg::Bank(BankMsg::Send {
            to_address: recipient.to_string(),
            amount: vec![self.deduct_tax(querier)?],
        }))
    }

    fn confirm_sent_native_balance(&self, message_info: &MessageInfo) -> StdResult<()> {
        // Accept EITHER bank denom as attached funds. The creator token is
        // now a TokenFactory native denom, so a `SimpleSwap` selling the
        // creator token attaches that denom directly (replacing the old
        // CW20 `Receive`/`Send` hook path). Whichever side the offer is,
        // `must_pay` verifies the exact attached amount.
        let denom = match &self.info {
            TokenType::Native { denom } | TokenType::CreatorToken { denom } => denom,
        };
        let amount =
            must_pay(message_info, denom).map_err(|err| StdError::generic_err(err.to_string()))?;
        if self.amount == amount {
            Ok(())
        } else {
            Err(StdError::generic_err(format!(
                "amount mismatch for denom '{}': expected {}, but received {}",
                denom, self.amount, amount
            )))
        }
    }
}
