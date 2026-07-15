//! Test-only querier that answers the poolmanager `EstimateSwapExactAmountIn`
//! Stargate query (FIX A) plus the factory USD-valuation wasm queries.
//!
//! The stock `cosmwasm_std::testing::MockQuerier` returns
//! `UnsupportedRequest` for `QueryRequest::Stargate` and offers no override
//! hook, so the on-chain swap-estimate floor introduced in FIX A cannot be
//! exercised through it. This wrapper decodes the osmosis-std estimate
//! request and returns a CONFIGURABLE expected-out amount
//! (`token_out = token_in * estimate_num / estimate_den`, default 1:1), so
//! swap/commit unit tests can assert the derived non-zero
//! `token_out_min_amount` in the emitted `MsgSwapExactAmountIn`.
//!
//! Bank-balance queries delegate to the wrapped `MockQuerier`; the factory
//! oracle (`ConvertNativeToUsd` / `CommitContext`) is answered inline so the
//! post-threshold commit swap path (which values the commit in USD before
//! swapping) works against this querier too.

#![cfg(test)]
#![allow(deprecated)]

use std::str::FromStr;

use cosmwasm_std::testing::{MockApi, MockQuerier, MockStorage, MOCK_CONTRACT_ADDR};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Coin, ContractResult, Empty, OwnedDeps, Querier, QuerierResult,
    QueryRequest, SystemError, SystemResult, Uint128, WasmQuery,
};
use osmosis_std::types::osmosis::poolmanager::v1beta1::{
    EstimateSwapExactAmountInRequest, EstimateSwapExactAmountInResponse,
};
use prost::Message;

/// Stargate path osmosis-std emits for `EstimateSwapExactAmountIn`.
pub const ESTIMATE_QUERY_PATH: &str =
    "/osmosis.poolmanager.v1beta1.Query/EstimateSwapExactAmountIn";

pub struct PoolMockQuerier {
    base: MockQuerier<Empty>,
    /// `estimated_out = token_in_amount * estimate_num / estimate_den`.
    estimate_num: Uint128,
    estimate_den: Uint128,
    /// When `Some(rate)`, factory USD-valuation queries are answered at
    /// `rate` micro-USD per micro-native (1_000_000 = $1/token).
    factory_rate: Option<Uint128>,
    /// Live bluechip wallet returned in the `CommitContext` response.
    bluechip_wallet: Addr,
}

impl Querier for PoolMockQuerier {
    fn raw_query(&self, bin_request: &[u8]) -> QuerierResult {
        let request: QueryRequest<Empty> = match from_json(bin_request) {
            Ok(v) => v,
            Err(e) => {
                return SystemResult::Err(SystemError::InvalidRequest {
                    error: format!("Parsing query request: {}", e),
                    request: bin_request.into(),
                })
            }
        };
        self.handle_query(&request)
    }
}

impl PoolMockQuerier {
    pub fn new(base: MockQuerier<Empty>) -> Self {
        PoolMockQuerier {
            base,
            estimate_num: Uint128::one(),
            estimate_den: Uint128::one(),
            factory_rate: None,
            bluechip_wallet: Addr::unchecked("bluechip_treasury"),
        }
    }

    /// Set the estimate ratio: `estimated_out = token_in * num / den`.
    #[allow(dead_code)]
    pub fn set_estimate_ratio(&mut self, num: u128, den: u128) {
        self.estimate_num = Uint128::new(num);
        self.estimate_den = Uint128::new(den);
    }

    /// Install the factory USD oracle at `rate` micro-USD per micro-native,
    /// returning `bluechip_wallet` as the live protocol wallet.
    pub fn set_factory_oracle(&mut self, rate: Uint128, bluechip_wallet: &str) {
        self.factory_rate = Some(rate);
        self.bluechip_wallet = Addr::unchecked(bluechip_wallet);
    }

    /// Seed / overwrite a bank balance on the wrapped base querier.
    #[allow(dead_code)]
    pub fn set_balance(&mut self, addr: &str, balances: Vec<Coin>) {
        self.base.bank.update_balance(addr, balances);
    }

    fn handle_query(&self, request: &QueryRequest<Empty>) -> QuerierResult {
        match request {
            QueryRequest::Stargate { path, data } if path == ESTIMATE_QUERY_PATH => {
                let req = match EstimateSwapExactAmountInRequest::decode(data.as_slice()) {
                    Ok(r) => r,
                    Err(e) => {
                        return SystemResult::Err(SystemError::InvalidRequest {
                            error: format!("decode estimate request: {}", e),
                            request: Default::default(),
                        })
                    }
                };
                // Osmosis coin string is `{amount}{denom}`; take the numeric
                // prefix as the token_in amount.
                let amount_str: String =
                    req.token_in.chars().take_while(|c| c.is_ascii_digit()).collect();
                let token_in_amount = Uint128::from_str(&amount_str).unwrap_or_default();
                let out = token_in_amount.multiply_ratio(self.estimate_num, self.estimate_den);
                let resp = EstimateSwapExactAmountInResponse {
                    token_out_amount: out.to_string(),
                };
                SystemResult::Ok(ContractResult::Ok(to_json_binary(&resp).unwrap()))
            }
            QueryRequest::Wasm(WasmQuery::Smart { msg, .. }) => {
                #[cosmwasm_schema::cw_serde]
                enum WrapperProbe {
                    PoolFactoryQuery(pool_factory_interfaces::FactoryQueryMsg),
                }
                if let Some(rate) = self.factory_rate {
                    let usd_at_rate = |amount: Uint128| {
                        amount
                            .checked_mul(rate)
                            .unwrap()
                            .checked_div(Uint128::new(1_000_000))
                            .unwrap()
                    };
                    match from_json(msg) {
                        Ok(WrapperProbe::PoolFactoryQuery(
                            pool_factory_interfaces::FactoryQueryMsg::ConvertNativeToUsd { amount },
                        )) => {
                            let resp = pool_factory_interfaces::ConversionResponse {
                                amount: usd_at_rate(amount),
                                rate_used: rate,
                                timestamp: 0,
                            };
                            return SystemResult::Ok(ContractResult::Ok(
                                to_json_binary(&resp).unwrap(),
                            ));
                        }
                        Ok(WrapperProbe::PoolFactoryQuery(
                            pool_factory_interfaces::FactoryQueryMsg::CommitContext { amount },
                        )) => {
                            let resp = pool_factory_interfaces::CommitContextResponse {
                                amount: usd_at_rate(amount),
                                rate_used: rate,
                                timestamp: 0,
                                bluechip_wallet: self.bluechip_wallet.clone(),
                            };
                            return SystemResult::Ok(ContractResult::Ok(
                                to_json_binary(&resp).unwrap(),
                            ));
                        }
                        _ => {}
                    }
                }
                SystemResult::Err(SystemError::InvalidRequest {
                    error: "no other cross-contract queries expected".to_string(),
                    request: msg.clone(),
                })
            }
            _ => self.base.handle_query(request),
        }
    }
}

/// `OwnedDeps` backed by [`PoolMockQuerier`], with the contract's bank
/// balance seeded from `balances`. Configure the estimate ratio / factory
/// oracle via the returned `deps.querier` builder methods.
pub fn mock_deps_estimate(
    balances: &[Coin],
) -> OwnedDeps<MockStorage, MockApi, PoolMockQuerier> {
    let base = MockQuerier::new(&[(MOCK_CONTRACT_ADDR, balances)]);
    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: PoolMockQuerier::new(base),
        custom_query_type: Default::default(),
    }
}
