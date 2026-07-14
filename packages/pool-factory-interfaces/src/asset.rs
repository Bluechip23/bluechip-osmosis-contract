use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    to_json_binary, Addr, Api, BalanceResponse, BankQuery, QuerierWrapper, QueryRequest, StdError,
    StdResult, Uint128, WasmQuery,
};
use cw20::{BalanceResponse as Cw20BalanceResponse, Cw20QueryMsg};
use std::fmt::{self, Display, Formatter, Result};

#[cw_serde]
pub struct TokenInfo {
    pub info: TokenType,
    pub amount: Uint128,
}

impl fmt::Display for TokenInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", self.amount, self.info)
    }
}

impl TokenInfo {
    pub fn is_native_token(&self) -> bool {
        self.info.is_native_token()
    }
}

#[cw_serde]
pub enum TokenType {
    /// The creator token. Post-Osmosis-migration this is a native
    /// TokenFactory bank denom (`factory/{pool_addr}/{subdenom}`) minted
    /// and burned by the pool contract (the denom admin), NOT a CW20
    /// contract. It is kept as a SEPARATE variant from `Native` — even
    /// though both are now bank coins — so all "which side is bluechip vs
    /// creator" routing (index 0 = bluechip, index 1 = creator) keeps
    /// working structurally rather than by string-matching denoms.
    CreatorToken {
        denom: String,
    },
    /// Any native bank denom on the chain — bluechip itself (`ubluechip`),
    /// IBC-wrapped remote assets (e.g. `ibc/...` for ATOM), tokenfactory
    /// denoms, etc. The wire tag is `"bluechip"` via `#[serde(rename = ...)]`:
    /// on-chain serialized state, deploy scripts, and frontend integrations
    /// all encode this variant under that key, so the rename attribute must
    /// stay for them to round-trip without a coordinated migration.
    #[serde(rename = "bluechip")]
    Native {
        denom: String,
    },
}

impl fmt::Display for TokenType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TokenType::Native { denom } => write!(f, "{}", denom),
            TokenType::CreatorToken { denom } => write!(f, "{}", denom),
        }
    }
}

impl TokenType {
    /// Whether this side is a native bank coin (funds are ATTACHED to the
    /// message rather than pulled via a CW20 allowance).
    ///
    /// Post-migration BOTH variants are bank denoms, so this returns
    /// `true` for both. Callers historically used this to mean "is a bank
    /// coin so `info.funds` carries it" — that meaning now covers the
    /// creator token too (a `SimpleSwap` selling the creator denom, a
    /// deposit attaching the creator denom, etc.), so treating
    /// `CreatorToken` as native here is correct.
    ///
    /// NOTE: this method must NOT be used to decide "is this the bluechip
    /// side" — that routing is done by matching the `Native` variant
    /// explicitly or by the fixed pair index (bluechip @ 0, creator @ 1).
    pub fn is_native_token(&self) -> bool {
        match self {
            TokenType::Native { .. } => true,
            // The creator token is a TokenFactory bank denom now, so it is
            // a native coin for funds-handling purposes.
            TokenType::CreatorToken { .. } => true,
        }
    }

    pub fn query_pool(&self, querier: &QuerierWrapper, pool_addr: Addr) -> StdResult<Uint128> {
        match self {
            // Both sides are bank denoms now — a plain balance query.
            TokenType::CreatorToken { denom, .. } => {
                query_balance(querier, pool_addr, denom.to_string())
            }
            TokenType::Native { denom, .. } => query_balance(querier, pool_addr, denom.to_string()),
        }
    }

    /// Strict variant of `query_pool`. Both sides are native bank denoms
    /// now, so both propagate the underlying bank-query error via the `?`
    /// in `query_balance` (no swallow-to-zero). Retained as a distinct
    /// method so callers that documented a fail-closed requirement (e.g.
    /// the router's slippage assertion in `router::execution`) keep an
    /// explicit strict entry point.
    pub fn query_pool_strict(
        &self,
        querier: &QuerierWrapper,
        pool_addr: Addr,
    ) -> StdResult<Uint128> {
        match self {
            TokenType::CreatorToken { denom, .. } => {
                query_balance(querier, pool_addr, denom.to_string())
            }
            TokenType::Native { denom, .. } => query_balance(querier, pool_addr, denom.to_string()),
        }
    }

    pub fn equal(&self, asset: &TokenType) -> bool {
        match (self, asset) {
            (TokenType::CreatorToken { denom: a }, TokenType::CreatorToken { denom: b }) => a == b,
            (TokenType::Native { denom: a }, TokenType::Native { denom: b }) => a == b,
            _ => false,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            TokenType::Native { denom } => denom.as_bytes(),
            TokenType::CreatorToken { denom } => denom.as_bytes(),
        }
    }

    /// Validate the per-side shape of a `pool_token_info` entry.
    ///
    /// Both variants are bank denoms now, so both reject empty /
    /// whitespace-only denoms. The bank module on-chain would reject the
    /// same shape later, but doing the check here surfaces operator typos
    /// at the contract boundary rather than 48h later when an apply lands
    /// a malformed denom and every subsequent BankMsg reverts inside the
    /// bank module with an error nobody is watching for. (Cosmos-SDK's
    /// stricter `^[a-zA-Z][a-zA-Z0-9/:._-]{2,127}$` regex is enforced by
    /// the factory's `validate_pool_token_info`; here we only check the
    /// lowest bar so this trait method stays meaningful for any consuming
    /// entry point.)
    ///
    /// Centralized here so every caller (e.g. creator-pool
    /// `instantiate`) gets the same guard set without an asymmetric
    /// inline empty-denom check at one call site only.
    pub fn check(&self, _api: &dyn Api) -> StdResult<()> {
        match self {
            TokenType::Native { denom } | TokenType::CreatorToken { denom } => {
                if denom.trim().is_empty() {
                    return Err(cosmwasm_std::StdError::generic_err(
                        "Token denom must be non-empty",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cw_serde]
pub enum PoolPairType {
    Xyk {},
    Stable {},
}

impl Display for PoolPairType {
    fn fmt(&self, fmt: &mut Formatter) -> Result {
        match self {
            PoolPairType::Xyk {} => fmt.write_str("xyk"),
            PoolPairType::Stable {} => fmt.write_str("stable"),
        }
    }
}

pub fn native_asset(denom: String, amount: Uint128) -> TokenInfo {
    TokenInfo {
        info: TokenType::Native { denom },
        amount,
    }
}

pub fn token_asset(denom: String, amount: Uint128) -> TokenInfo {
    TokenInfo {
        info: TokenType::CreatorToken { denom },
        amount,
    }
}

// Extracts the native bluechip denom from a pool's asset_infos array.
pub fn get_native_denom(asset_infos: &[TokenType; 2]) -> StdResult<String> {
    for asset in asset_infos {
        if let TokenType::Native { denom } = asset {
            return Ok(denom.clone());
        }
    }
    Err(StdError::generic_err(
        "No bluechip (native) asset found in pool asset_infos",
    ))
}

/// Queries a CW20 token balance for a given account.
pub fn query_token_balance(
    querier: &QuerierWrapper,
    contract_addr: Addr,
    account_addr: Addr,
) -> StdResult<Uint128> {
    let res: Cw20BalanceResponse = querier
        .query(&QueryRequest::Wasm(WasmQuery::Smart {
            contract_addr: String::from(contract_addr),
            msg: to_json_binary(&Cw20QueryMsg::Balance {
                address: String::from(account_addr),
            })?,
        }))
        .unwrap_or_else(|_| Cw20BalanceResponse {
            balance: Uint128::zero(),
        });
    Ok(res.balance)
}

/// Strict variant of `query_token_balance`: propagates the underlying
/// query error instead of swallowing it as a zero balance.
///
/// Used by the deposit balance-verification path. There, swallowing
/// a failed pre-balance query as zero would let the post-balance query's
/// full pool reserve appear as a "delta" — silently masking the very
/// fee-on-transfer / rebasing CW20 corruption the verification is meant
/// to detect.
pub fn query_token_balance_strict(
    querier: &QuerierWrapper,
    contract_addr: &Addr,
    account_addr: &Addr,
) -> StdResult<Uint128> {
    let res: Cw20BalanceResponse = querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
        contract_addr: contract_addr.to_string(),
        msg: to_json_binary(&Cw20QueryMsg::Balance {
            address: account_addr.to_string(),
        })?,
    }))?;
    Ok(res.balance)
}

/// Queries a native bank balance for a given account and denom.
pub fn query_balance(
    querier: &QuerierWrapper,
    account_addr: Addr,
    denom: String,
) -> StdResult<Uint128> {
    let balance: BalanceResponse = querier.query(&QueryRequest::Bank(BankQuery::Balance {
        address: String::from(account_addr),
        denom,
    }))?;
    Ok(balance.amount.amount)
}

/// Queries the current token balances for a pair of asset types at a given contract address.
pub fn query_pools(
    asset_infos: &[TokenType; 2],
    querier: &QuerierWrapper,
    contract_addr: Addr,
) -> StdResult<[TokenInfo; 2]> {
    Ok([
        TokenInfo {
            amount: asset_infos[0].query_pool(querier, contract_addr.clone())?,
            info: asset_infos[0].clone(),
        },
        TokenInfo {
            amount: asset_infos[1].query_pool(querier, contract_addr)?,
            info: asset_infos[1].clone(),
        },
    ])
}
