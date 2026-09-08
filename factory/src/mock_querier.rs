#![cfg(not(target_arch = "wasm32"))]

use cosmwasm_std::testing::{MockApi, MockQuerier, MockStorage, MOCK_CONTRACT_ADDR};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Coin, Empty, OwnedDeps, Querier, QuerierResult,
    QueryRequest, SystemError, SystemResult, WasmQuery,
};
use pool_factory_interfaces::{IsPausedResponse, PoolQueryMsg, PoolStateResponseForFactory};

use crate::query::QueryMsg;

/// Default mock fee-route TWAP: "3.0" native base units per fee base unit —
/// shaped like the real osmosis-1 relation (~600 uosmo per 20 uUSDC would be
/// 30; 3.0 keeps fixture budgets small). Tests that exercise budget math set
/// their own value via `set_twap`.
pub const DEFAULT_MOCK_TWAP_PRICE: &str = "3.0";

pub fn mock_dependencies(
    contract_balance: &[Coin],
) -> OwnedDeps<MockStorage, MockApi, WasmMockQuerier> {
    let custom_querier: WasmMockQuerier =
        WasmMockQuerier::new(MockQuerier::new(&[(MOCK_CONTRACT_ADDR, contract_balance)]));

    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: custom_querier,
        custom_query_type: Default::default(),
    }
}

pub struct WasmMockQuerier {
    base: MockQuerier<Empty>,
    pub paused_pools: std::collections::HashSet<String>,
    // Pool addresses whose queries should hard-error. Used to exercise the
    // factory's graceful-fallback behavior when a pool contract is broken
    // or has been migrated out from under the factory.
    pub query_error_pools: std::collections::HashSet<String>,
    // Per-pool overrides for `PoolQueryMsg::GetPoolState`. Keyed by
    // contract address; when present, the override is returned verbatim.
    // Falls back to the default 50B/10B reserves below if no override is
    // registered for the queried address.
    pub pool_state_overrides: std::collections::HashMap<String, PoolStateResponseForFactory>,
    // Decimal-string price served for the fee-route arithmetic-TWAP
    // stargate query (`/osmosis.twap.v1beta1.Query/ArithmeticTwapToNow`),
    // native base units per fee base unit. Err(reason) makes the query fail
    // the way a missing/pruned pricing pool does (factory fails closed).
    pub twap_result: Result<String, String>,
    // (denom, amount) served as the live x/poolmanager pool-creation fee;
    // None models a chain/mock without the params query.
    pub live_pool_creation_fee: Option<(String, u128)>,
}

impl Querier for WasmMockQuerier {
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

impl WasmMockQuerier {
    // `QueryRequest::Stargate` is deprecated upstream in favor of `Grpc`,
    // but it is the variant osmosis-std 0.27 emits, so it's what the
    // mock must answer.
    pub fn handle_query(&self, request: &QueryRequest<Empty>) -> QuerierResult {
        match &request {
            QueryRequest::Wasm(WasmQuery::Smart { contract_addr, msg }) => {
                // Hard failure path — lets tests verify fallback behavior.
                if self.query_error_pools.contains(contract_addr.as_str()) {
                    return SystemResult::Err(SystemError::NoSuchContract {
                        addr: contract_addr.clone(),
                    });
                }
                // Try parsing as PoolQueryMsg (for pool contract queries)
                if let Ok(pool_msg) = from_json::<PoolQueryMsg>(&msg) {
                    match pool_msg {
                        PoolQueryMsg::GetPoolState {} => {
                            // Per-pool override takes precedence — tests
                            // that need distinct reserves per pool register
                            // them via `pool_state_overrides`. Fall back to
                            // the default 50B/10B numbers (which most
                            // existing tests rely on) when no override is
                            // registered for this address.
                            let pool_state = if let Some(override_state) =
                                self.pool_state_overrides.get(contract_addr.as_str())
                            {
                                override_state.clone()
                            } else {
                                PoolStateResponseForFactory {
                                    pool_contract_address: Addr::unchecked(contract_addr.clone()),
                                    nft_ownership_accepted: true,
                                    reserve0: cosmwasm_std::Uint128::new(50_000_000_000),
                                    reserve1: cosmwasm_std::Uint128::new(10_000_000_000),
                                    total_liquidity: cosmwasm_std::Uint128::new(10_000_000),
                                    block_time_last: 0,
                                    price0_cumulative_last: cosmwasm_std::Uint128::zero(),
                                    price1_cumulative_last: cosmwasm_std::Uint128::zero(),
                                    assets: vec![],
                                }
                            };
                            return SystemResult::Ok(to_json_binary(&pool_state).into());
                        }
                        PoolQueryMsg::IsPaused {} => {
                            // Tests can mark specific pools as paused by
                            // inserting their address into `paused_pools`.
                            let paused = self.paused_pools.contains(contract_addr.as_str());
                            return SystemResult::Ok(
                                to_json_binary(&IsPausedResponse { paused }).into(),
                            );
                        }
                        _ => {
                            return SystemResult::Err(SystemError::InvalidRequest {
                                error: "Unsupported pool query".to_string(),
                                request: msg.clone(),
                            })
                        }
                    }
                }

                if let Ok(_factory_msg) = from_json::<QueryMsg>(&msg) {
                    panic!("Unsupported factory query");
                }

                // If neither parse succeeded
                SystemResult::Err(SystemError::InvalidRequest {
                    error: "Could not parse query message".to_string(),
                    request: msg.clone(),
                })
            }
            // osmosis-std 0.27 emits `QueryRequest::Stargate` for chain-module
            // queries. The only one the factory makes is the fee-route
            // arithmetic TWAP; serve the configured decimal string (or fail
            // like a missing pricing pool would).
            #[allow(deprecated)]
            QueryRequest::Stargate { path, .. } => {
                if path.contains("poolmanager") && path.ends_with("Params") {
                    // Live x/poolmanager pool-creation fee. Unset (the
                    // default) models a chain/mock without the query —
                    // CommitContext then falls back to the configured fee.
                    return match &self.live_pool_creation_fee {
                        Some((denom, amount)) => SystemResult::Ok(
                            to_json_binary(
                                &osmosis_std::types::osmosis::poolmanager::v1beta1::ParamsResponse {
                                    params: Some(
                                        osmosis_std::types::osmosis::poolmanager::v1beta1::Params {
                                            pool_creation_fee: vec![
                                                osmosis_std::types::cosmos::base::v1beta1::Coin {
                                                    denom: denom.clone(),
                                                    amount: amount.to_string(),
                                                },
                                            ],
                                            taker_fee_params: None,
                                            authorized_quote_denoms: vec![],
                                        },
                                    ),
                                },
                            )
                            .into(),
                        ),
                        None => SystemResult::Err(SystemError::UnsupportedRequest {
                            kind: "poolmanager params not configured in mock".to_string(),
                        }),
                    };
                }
                if path.contains("twap") && path.contains("ArithmeticTwapToNow") {
                    return match &self.twap_result {
                        Ok(price) => SystemResult::Ok(
                            to_json_binary(
                                &osmosis_std::types::osmosis::twap::v1beta1::ArithmeticTwapToNowResponse {
                                    arithmetic_twap: price.clone(),
                                },
                            )
                            .into(),
                        ),
                        Err(reason) => SystemResult::Err(SystemError::InvalidRequest {
                            error: reason.clone(),
                            request: Default::default(),
                        }),
                    };
                }
                SystemResult::Err(SystemError::UnsupportedRequest {
                    kind: format!("stargate path {path}"),
                })
            }
            _ => self.base.handle_query(request),
        }
    }
}

impl WasmMockQuerier {
    pub fn new(base: MockQuerier<Empty>) -> Self {
        WasmMockQuerier {
            base,
            paused_pools: std::collections::HashSet::new(),
            query_error_pools: std::collections::HashSet::new(),
            pool_state_overrides: std::collections::HashMap::new(),
            twap_result: Ok(DEFAULT_MOCK_TWAP_PRICE.to_string()),
            live_pool_creation_fee: None,
        }
    }

    /// Serve a live x/poolmanager pool-creation fee (denom, amount) so
    /// CommitContext's live-fee sizing path can be exercised in unit tests.
    #[allow(dead_code)]
    pub fn set_live_pool_creation_fee(&mut self, denom: &str, amount: u128) {
        self.live_pool_creation_fee = Some((denom.to_string(), amount));
    }

    /// Serve `price` (decimal string, native per fee unit) for the
    /// fee-route TWAP query.
    pub fn set_twap(&mut self, price: &str) {
        self.twap_result = Ok(price.to_string());
    }

    /// Make the fee-route TWAP query fail (models a missing/pruned pricing
    /// pool) so factory validation and CommitContext fail closed.
    pub fn set_twap_error(&mut self, reason: &str) {
        self.twap_result = Err(reason.to_string());
    }

    /// Register an explicit `PoolStateResponseForFactory` for a given
    /// contract address. Subsequent `GetPoolState` queries against that
    /// address will return the override verbatim, bypassing the default
    /// 50B/10B response. For integration tests that need to model
    /// drained / lopsided / healthy pools side-by-side.
    #[allow(dead_code)]
    pub fn set_pool_state(&mut self, addr: &str, state: PoolStateResponseForFactory) {
        self.pool_state_overrides.insert(addr.to_string(), state);
    }
}
