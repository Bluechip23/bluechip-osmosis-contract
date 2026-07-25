#![cfg(not(target_arch = "wasm32"))]

use cosmwasm_std::testing::{MockApi, MockQuerier, MockStorage, MOCK_CONTRACT_ADDR};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Coin, Empty, Int64, OwnedDeps, Querier, QuerierResult,
    QueryRequest, SystemError, SystemResult, Uint64, WasmQuery,
};
use pool_factory_interfaces::{IsPausedResponse, PoolQueryMsg, PoolStateResponseForFactory};

use crate::pyth_types::{PriceFeed, PriceFeedResponse, PythPriceRetrievalResponse, PythQueryMsg};
use crate::query::QueryMsg;

/// Default mock Pyth reading: $1.00 per native at expo -6 (price 1e6),
/// zero confidence, and a publish_time 30s before the default `mock_env`
/// block time (1_571_797_419) so the staleness gate ([10, 300]s) passes.
/// Most existing factory tests instantiate the factory (which runs the
/// live Pyth probe) with the default `mock_env`, so this keeps them green.
pub const DEFAULT_MOCK_PYTH_PRICE: i64 = 1_000_000;
pub const DEFAULT_MOCK_PYTH_EXPO: i32 = -6;
pub const DEFAULT_MOCK_PYTH_PUBLISH_TIME: u64 = 1_571_797_389;

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
    // Result served for the Pyth `PriceFeed` smart query the factory's
    // `usd_price::probe_pyth_usd_rate` emits. Ok((price, expo, conf)) is
    // returned as the feed's price; Err(reason) makes the query fail the
    // way an unreachable/misconfigured Pyth contract does. Defaults to
    // $1.00 fresh.
    pub pyth_result: Result<(i64, i32, u64), String>,
    // Absolute publish_time returned for the Pyth feed. Tests set this
    // relative to their env block time to exercise the staleness / min-age
    // / future-skew gates. Defaults to `DEFAULT_MOCK_PYTH_PUBLISH_TIME`.
    pub pyth_publish_time: u64,
    // When set, the mock returns this feed id in the response INSTEAD of the
    // requested one — models a mis-routing Pyth contract so the factory's
    // feed-id-match gate can be exercised.
    pub pyth_feed_id_override: Option<String>,
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
                // Pyth `PriceFeed` query (the factory's live USD price read).
                if let Ok(PythQueryMsg::PriceFeed { id }) = from_json::<PythQueryMsg>(msg) {
                    return match &self.pyth_result {
                        Ok((price, expo, conf)) => {
                            let retr = PythPriceRetrievalResponse {
                                price: Int64::new(*price),
                                conf: Uint64::new(*conf),
                                expo: *expo,
                                publish_time: self.pyth_publish_time as i64,
                            };
                            // Return the requested id by default; a test can
                            // force a DIFFERENT id (models a mis-routing Pyth
                            // contract) to exercise the feed-id-match gate.
                            let resp_id = self
                                .pyth_feed_id_override
                                .clone()
                                .unwrap_or(id);
                            let resp = PriceFeedResponse {
                                price_feed: Some(PriceFeed {
                                    id: resp_id,
                                    price: retr.clone(),
                                    ema_price: retr,
                                }),
                                price: None,
                            };
                            SystemResult::Ok(to_json_binary(&resp).into())
                        }
                        Err(reason) => SystemResult::Err(SystemError::InvalidRequest {
                            error: reason.clone(),
                            request: Default::default(),
                        }),
                    };
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
            pyth_result: Ok((
                DEFAULT_MOCK_PYTH_PRICE,
                DEFAULT_MOCK_PYTH_EXPO,
                0,
            )),
            pyth_publish_time: DEFAULT_MOCK_PYTH_PUBLISH_TIME,
            pyth_feed_id_override: None,
        }
    }

    /// Force the mock to answer with a DIFFERENT feed id than requested
    /// (models a mis-routing Pyth contract → the factory must reject).
    pub fn set_pyth_feed_id_override(&mut self, id: &str) {
        self.pyth_feed_id_override = Some(id.to_string());
    }

    /// Serve a Pyth price of `usd_micro` micro-USD per native token
    /// (expo -6, so `usd_micro == rate_used`), zero confidence, at the
    /// default publish_time. e.g. `1_000_000` == $1.00.
    pub fn set_pyth_usd_micro(&mut self, usd_micro: i64) {
        self.pyth_result = Ok((usd_micro, -6, 0));
    }

    /// Serve an explicit Pyth `(price, expo, conf)` triple.
    pub fn set_pyth_price(&mut self, price: i64, expo: i32, conf: u64) {
        self.pyth_result = Ok((price, expo, conf));
    }

    /// Set the Pyth feed's absolute publish_time (unix seconds). Tests set
    /// this relative to their env block time to exercise the staleness /
    /// min-age / future-skew gates.
    pub fn set_pyth_publish_time(&mut self, t: u64) {
        self.pyth_publish_time = t;
    }

    /// Make the Pyth query fail (models an unreachable / misconfigured
    /// Pyth contract) so the factory's valuation fails closed.
    pub fn set_pyth_error(&mut self, reason: &str) {
        self.pyth_result = Err(reason.to_string());
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
