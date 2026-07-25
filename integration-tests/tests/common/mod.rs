//! Shared helpers for the osmosis-test-tube E2E suites.
//!
//! Everything here runs against the REAL Osmosis modules (gamm /
//! poolmanager / tokenfactory / x-twap) embedded by `osmosis-test-tube`;
//! nothing is mocked. Keep the version-sensitive runner surface confined
//! to this module plus the `tt`-style helpers, mirroring
//! `lifecycle.rs`.

#![allow(dead_code)]

use cosmwasm_std::{Coin, Uint128};

use factory::msg::{CreatorTokenInfo, ExecuteMsg as FactoryExecuteMsg};
use factory::pool_struct::{CreatePool, ThresholdPayoutAmounts};
use factory::query::PoolsResponse;
use factory::state::FactoryInstantiate;
use pool_factory_interfaces::asset::{TokenInfo, TokenType};

use creator_pool::msg::{ExecuteMsg as PoolExecuteMsg, QueryMsg as PoolQueryMsg};

use osmosis_test_tube::osmosis_std::types::cosmos::bank::v1beta1::QueryBalanceRequest;
use osmosis_test_tube::{Account, Bank, Module, OsmosisTestApp, SigningAccount, Wasm};

pub const UOSMO: &str = "uosmo";
pub const UUSDC: &str = "uusdc";

/// $25,000, 6-dec USD — same threshold the lifecycle suite uses.
pub const THRESHOLD_USD: u128 = 25_000_000_000;
/// Osmosis default pool-creation fee: 1000 OSMO.
pub const GAMM_CREATE_FEE: u128 = 1_000_000_000;
/// The (mock) Pyth OSMO/USD feed id used across the harness.
pub const OSMO_USD_FEED: &str =
    "5867f5683c757393a0670ef0f701490950fe93fdb006d181c8265a831ac0c5c6";
/// Seconds to age a freshly-pushed Pyth price past the factory's
/// `MIN_PYTH_AGE_SECONDS` (10s) floor before it can be consumed.
pub const PYTH_AGE_BUMP: u64 = 15;

pub fn read_wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../artifacts/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!("missing wasm artifact {path}: {e}. Build it first (see README).")
    })
}

/// Store the factory + pool wasm and return `(factory_code_id, pool_code_id)`.
pub fn store_factory_and_pool(wasm: &Wasm<OsmosisTestApp>, signer: &SigningAccount) -> (u64, u64) {
    let factory_code_id = wasm
        .store_code(&read_wasm("factory.wasm"), None, signer)
        .unwrap()
        .data
        .code_id;
    let pool_code_id = wasm
        .store_code(&read_wasm("pool.wasm"), None, signer)
        .unwrap()
        .data
        .code_id;
    (factory_code_id, pool_code_id)
}

/// Store the mock Pyth oracle wasm and return its code id.
pub fn store_mock_pyth(wasm: &Wasm<OsmosisTestApp>, signer: &SigningAccount) -> u64 {
    wasm.store_code(&read_wasm("mock_pyth.wasm"), None, signer)
        .unwrap()
        .data
        .code_id
}

/// Instantiate the mock Pyth oracle and return its address.
pub fn instantiate_mock_pyth(
    wasm: &Wasm<OsmosisTestApp>,
    code_id: u64,
    admin: &SigningAccount,
) -> String {
    wasm.instantiate(
        code_id,
        &mock_pyth::InstantiateMsg {},
        Some(&admin.address()),
        Some("mock-pyth"),
        &[],
        admin,
    )
    .unwrap()
    .data
    .address
}

/// Push an OSMO/USD price of `usd_micro` micro-USD (expo -6, so
/// `usd_micro == rate_used`) to the mock Pyth, stamped at the current
/// block time, then advance the chain `PYTH_AGE_BUMP` seconds so it clears
/// the factory's MIN_PYTH_AGE floor. Call right before any commit so the
/// price is fresh (age 15s) at valuation time.
pub fn refresh_pyth(
    app: &OsmosisTestApp,
    wasm: &Wasm<OsmosisTestApp>,
    pyth_addr: &str,
    signer: &SigningAccount,
    usd_micro: i64,
) {
    wasm.execute(
        pyth_addr,
        &mock_pyth::ExecuteMsg::SetPrice {
            price_id: OSMO_USD_FEED.to_string(),
            price: usd_micro,
            expo: -6,
            conf: 0,
        },
        &[],
        signer,
    )
    .unwrap();
    app.increase_time(PYTH_AGE_BUMP);
}

/// Set a full explicit Pyth reading (price/expo/conf/publish_time) for the
/// fail-closed E2E cases (staleness / future-skew / wide-confidence).
pub fn set_pyth_at(
    wasm: &Wasm<OsmosisTestApp>,
    pyth_addr: &str,
    signer: &SigningAccount,
    price: i64,
    expo: i32,
    conf: u64,
    publish_time: i64,
) {
    wasm.execute(
        pyth_addr,
        &mock_pyth::ExecuteMsg::SetPriceAt {
            price_id: OSMO_USD_FEED.to_string(),
            price,
            expo,
            conf,
            publish_time,
        },
        &[],
        signer,
    )
    .unwrap();
}

/// Factory config wired to the (mock) Pyth oracle at `pyth_addr`, valuing
/// the native asset via the OSMO/USD feed. `pricing_pool_id` survives only
/// as the cross-denom fee-swap execution route.
pub fn factory_init(
    admin: &str,
    pricing_pool_id: u64,
    pool_code_id: u64,
    gamm_pool_creation_fee: Coin,
    pyth_addr: &str,
) -> FactoryInstantiate {
    FactoryInstantiate {
        factory_admin_address: cosmwasm_std::Addr::unchecked(admin),
        commit_threshold_limit_usd: Uint128::new(THRESHOLD_USD),
        cw20_token_contract_id: pool_code_id,
        cw721_nft_contract_id: pool_code_id,
        create_pool_wasm_contract_id: pool_code_id,
        bluechip_wallet_address: cosmwasm_std::Addr::unchecked(admin),
        commit_fee_bluechip: cosmwasm_std::Decimal::percent(1),
        commit_fee_creator: cosmwasm_std::Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(30_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: UOSMO.to_string(),
        pricing_pool_id,
        usd_quote_denom: UUSDC.to_string(),
        pool_creation_fee: Uint128::zero(),
        gamm_pool_creation_fee,
        threshold_payout_amounts: ThresholdPayoutAmounts::default(),
        emergency_withdraw_delay_seconds: 86_400,
        pyth_contract_addr: pyth_addr.to_string(),
        pyth_native_usd_feed_id: OSMO_USD_FEED.to_string(),
        max_pyth_staleness_seconds: 600,
        pyth_conf_threshold_bps: 200,
    }
}

/// Instantiate a factory and return its address.
pub fn instantiate_factory(
    wasm: &Wasm<OsmosisTestApp>,
    factory_code_id: u64,
    init: &FactoryInstantiate,
    admin: &SigningAccount,
) -> String {
    wasm.instantiate(
        factory_code_id,
        init,
        Some(&admin.address()),
        Some("factory"),
        &[],
        admin,
    )
    .unwrap()
    .data
    .address
}

/// Create a commit pool via the factory and return
/// `(pool_addr, creator_denom, registry_pool_id)`.
pub fn create_creator_pool(
    wasm: &Wasm<OsmosisTestApp>,
    factory_addr: &str,
    creator: &SigningAccount,
    symbol: &str,
) -> (String, String, u64) {
    let create = FactoryExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: UOSMO.to_string(),
                },
                TokenType::CreatorToken {
                    denom: "WILL_BE_CREATED_BY_FACTORY".to_string(),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: format!("{symbol} Creator Token"),
            symbol: symbol.to_string(),
            decimal: 6,
        },
    };
    wasm.execute(factory_addr, &create, &[], creator).unwrap();

    let pools: PoolsResponse = wasm
        .query(
            factory_addr,
            &factory::query::QueryMsg::Pools {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    let entry = pools.pools.iter().last().expect("pool registered");
    let pool_addr = entry.pool_addr.to_string();
    let denom = format!("factory/{}/{}", pool_addr, symbol.to_lowercase());
    (pool_addr, denom, entry.pool_id)
}

/// A `Commit` execute message for `amount` uosmo.
pub fn commit_msg(amount: u128, belief_price: Option<cosmwasm_std::Decimal>) -> PoolExecuteMsg {
    PoolExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(amount),
        },
        transaction_deadline: None,
        belief_price,
        max_spread: None,
    }
}

/// Native bank balance of `denom` held by `address`.
pub fn balance(bank: &Bank<OsmosisTestApp>, address: &str, denom: &str) -> Uint128 {
    let resp = bank
        .query_balance(&QueryBalanceRequest {
            address: address.to_string(),
            denom: denom.to_string(),
        })
        .unwrap();
    resp.balance
        .map(|c| c.amount.parse::<u128>().unwrap_or(0))
        .map(Uint128::new)
        .unwrap_or_default()
}

/// Native bank balance without needing a caller-held `Bank` handle.
pub fn tt_balance(app: &OsmosisTestApp, address: &str, denom: &str) -> Uint128 {
    let bank = Bank::new(app);
    balance(&bank, address, denom)
}


/// The factory's live native→USD conversion for `amount` base units —
/// the EXACT query every commit's valuation runs through.
pub fn convert_native_to_usd(
    wasm: &Wasm<OsmosisTestApp>,
    factory_addr: &str,
    amount: u128,
) -> Result<pool_factory_interfaces::ConversionResponse, osmosis_test_tube::RunnerError> {
    wasm.query(
        factory_addr,
        &factory::query::QueryMsg::PoolFactoryQuery(
            pool_factory_interfaces::FactoryQueryMsg::ConvertNativeToUsd {
                amount: Uint128::new(amount),
            },
        ),
    )
}

/// Drain a pool's post-crossing distribution completely (batched,
/// rate-limited on-chain at 5s per call).
pub fn drain_distribution(
    app: &OsmosisTestApp,
    wasm: &Wasm<OsmosisTestApp>,
    pool_addr: &str,
    caller: &SigningAccount,
) {
    loop {
        wasm.execute(
            pool_addr,
            &PoolExecuteMsg::ContinueDistribution {},
            &[],
            caller,
        )
        .unwrap();
        let ds: Option<creator_pool::msg::DistributionStateResponse> = wasm
            .query(pool_addr, &PoolQueryMsg::DistributionState {})
            .unwrap();
        match ds {
            Some(s) if s.is_distributing => {
                app.increase_time(6);
                continue;
            }
            _ => break,
        }
    }
}

/// Set the chain's `x/poolmanager` pool-creation fee (read-modify-write so
/// unrelated params survive) — the knob the crossing's fee logic reads live.
pub fn set_pool_creation_fee(app: &OsmosisTestApp, denom: &str, amount: u128) {
    use osmosis_test_tube::cosmrs::Any;
    use osmosis_test_tube::osmosis_std::types::osmosis::poolmanager::v1beta1::Params as PmParams;
    use prost::Message;

    let mut pm_params: PmParams = app
        .get_param_set("poolmanager", PmParams::TYPE_URL)
        .expect("read poolmanager params");
    pm_params.pool_creation_fee = vec![
        osmosis_test_tube::osmosis_std::types::cosmos::base::v1beta1::Coin {
            denom: denom.to_string(),
            amount: amount.to_string(),
        },
    ];
    app.set_param_set(
        "poolmanager",
        Any {
            type_url: PmParams::TYPE_URL.to_string(),
            value: pm_params.encode_to_vec(),
        },
    )
    .expect("set poolmanager params");
}

/// Isolate the pricing pools from Osmosis's two automatic cross-pool
/// side-effects so a manipulation swap in an oracle test moves ONLY its
/// target pool. **Call before creating any pool** (the taker fee is
/// captured per-pool at creation).
///
/// Two mechanisms otherwise couple unrelated pools to the OSMO/USDC
/// pricing pool, and both would silently perturb the primary source when a
/// test manipulates some other pair:
///
/// 1. **x/protorev (in-protocol arbitrage / MEV capture).** On every swap
///    Osmosis searches for a profitable cyclic arbitrage and executes a
///    backrun with module funds. Manipulating a leg pool creates exactly
///    such a dislocation, and protorev's corrective cycle routes through
///    the OSMO/USDC anchor — so the primary pricing pool moves even though
///    the test never swapped on it. (This is real mainnet behaviour and it
///    actually *helps* the oracle — it partially self-corrects
///    manipulation — but it makes an isolated unit-under-test
///    non-deterministic, so tests disable it.)
/// 2. **Taker fee auto-conversion.** A taker fee collected in a non-OSMO
///    denom is swapped to OSMO through an OSMO pool, again touching the
///    anchor.
///
/// Disabling protorev + zeroing the taker fee removes both couplings.
pub fn isolate_pricing_pools(app: &OsmosisTestApp) {
    use osmosis_test_tube::cosmrs::Any;
    use osmosis_test_tube::osmosis_std::types::osmosis::poolmanager::v1beta1::Params as PmParams;
    use osmosis_test_tube::osmosis_std::types::osmosis::protorev::v1beta1::Params as PrParams;
    use prost::Message;

    // (1) Disable protorev.
    if let Ok(mut pr) =
        app.get_param_set::<PrParams>("protorev", "/osmosis.protorev.v1beta1.Params")
    {
        pr.enabled = false;
        app.set_param_set(
            "protorev",
            Any {
                type_url: "/osmosis.protorev.v1beta1.Params".to_string(),
                value: pr.encode_to_vec(),
            },
        )
        .expect("disable protorev");
    }

    // (2) Zero the default taker fee (sdk.Dec serialized as its raw
    // 18-decimal integer, so "0" is zero).
    let mut pm_params: PmParams = app
        .get_param_set("poolmanager", PmParams::TYPE_URL)
        .expect("read poolmanager params");
    if let Some(tf) = pm_params.taker_fee_params.as_mut() {
        tf.default_taker_fee = "0".to_string();
    }
    app.set_param_set(
        "poolmanager",
        Any {
            type_url: PmParams::TYPE_URL.to_string(),
            value: pm_params.encode_to_vec(),
        },
    )
    .expect("set poolmanager params (zero taker fee)");
}

/// Swap `amount_in` of `denom_in` for `denom_out` DIRECTLY on gamm pool
/// `pool_id` via the poolmanager — the tool used to manipulate a pricing
/// pool's price mid-test the way a real attacker would.
pub fn raw_pool_swap(
    app: &OsmosisTestApp,
    pool_id: u64,
    denom_in: &str,
    amount_in: u128,
    denom_out: &str,
    signer: &SigningAccount,
) {
    use osmosis_test_tube::osmosis_std::types::cosmos::base::v1beta1::Coin as ProtoCoin;
    use osmosis_test_tube::osmosis_std::types::osmosis::poolmanager::v1beta1::{
        MsgSwapExactAmountIn, MsgSwapExactAmountInResponse, SwapAmountInRoute,
    };
    use osmosis_test_tube::Runner;

    let msg = MsgSwapExactAmountIn {
        sender: signer.address(),
        routes: vec![SwapAmountInRoute {
            pool_id,
            token_out_denom: denom_out.to_string(),
        }],
        token_in: Some(ProtoCoin {
            denom: denom_in.to_string(),
            amount: amount_in.to_string(),
        }),
        token_out_min_amount: "1".to_string(),
    };
    app.execute::<_, MsgSwapExactAmountInResponse>(msg, MsgSwapExactAmountIn::TYPE_URL, signer)
        .unwrap();
}

/// Sorted-coin convenience: Cosmos requires denom-sorted coin lists.
pub fn coins_sorted(mut coins: Vec<Coin>) -> Vec<Coin> {
    coins.sort_by(|a, b| a.denom.cmp(&b.denom));
    coins
}
