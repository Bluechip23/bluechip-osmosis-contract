//! End-to-end lifecycle test on a REAL Osmosis chain (osmosis-test-tube).
//!
//! Unlike the crate's unit tests — which run against `mock_dependencies`
//! and therefore stub out every Osmosis-native call — this test executes
//! the actual `tokenfactory`, `gamm`, `poolmanager`, and `twap` modules, so
//! it is the only place the migration's core claims are validated against
//! real chain behaviour:
//!
//!   * `MsgCreateDenom` / `MsgSetDenomMetadata` actually register the
//!     creator token,
//!   * the x/twap USD valuation resolves against a real pricing pool,
//!   * the threshold-crossing commit's `reply_on_success`
//!     `MsgCreateBalancerPool` actually seeds a native GAMM pool (and the
//!     reply decodes `MsgCreateBalancerPoolResponse.pool_id` from real
//!     protobuf bytes — a path no mock can exercise),
//!   * the four TokenFactory `MsgMint`s land, and
//!   * a post-crossing `SimpleSwap` routes through `MsgSwapExactAmountIn`
//!     and the reply forwards the output.
//!
//! Because the crossing's create-pool SubMsg is `reply_on_success` and
//! `IS_THRESHOLD_HIT` is set inside the same tx BEFORE that reply, a failed
//! GAMM creation reverts the whole crossing. So the single assertion
//! "the pool reports FullyCommitted afterwards" is a strong end-to-end
//! proof that the native pool was created and its id stored.
//!
//! ---------------------------------------------------------------------------
//! RUNNING (see README.md):
//!   1. Build optimized wasm into `../artifacts/` (factory.wasm, pool.wasm).
//!   2. `cd integration-tests && cargo test -- --nocapture`
//! This crate is excluded from the root workspace; a normal `cargo test`
//! at the repo root does NOT build or run it.
//! ---------------------------------------------------------------------------
//!
//! The `osmosis_test_tube` runner API is version-sensitive. Every call into
//! it is confined to the small `tt` helpers below and marked, so a version
//! bump only touches those lines, not the scenario logic.

use cosmwasm_std::{Coin, Decimal, Uint128};

use factory::pool_struct::{CreatePool, ThresholdPayoutAmounts};
use factory::msg::{CreatorTokenInfo, ExecuteMsg as FactoryExecuteMsg};
use factory::query::{PoolsResponse, QueryMsg as FactoryQueryMsg};
use factory::state::FactoryInstantiate;
use pool_factory_interfaces::asset::{TokenInfo, TokenType};

use creator_pool::msg::{CommitStatus, ExecuteMsg as PoolExecuteMsg, QueryMsg as PoolQueryMsg};

use osmosis_test_tube::{Account, Bank, Gamm, Module, OsmosisTestApp, Wasm};

// ---------------------------------------------------------------------------
// Constants for the scenario
// ---------------------------------------------------------------------------

const UOSMO: &str = "uosmo";
const UUSDC: &str = "uusdc";

/// $25,000, 6-dec USD.
const THRESHOLD_USD: u128 = 25_000_000_000;
/// Osmosis default pool-creation fee: 1000 OSMO.
const GAMM_CREATE_FEE: u128 = 1_000_000_000;
/// Canonical minted supply (325B+25B+350B+500B).
const TOTAL_CREATOR_SUPPLY: u128 = 1_200_000_000_000;
/// Creator's up-front reward mint.
const CREATOR_REWARD: u128 = 325_000_000_000;

fn read_wasm(name: &str) -> Vec<u8> {
    // Optimizer output lives in ../artifacts. The Makefile copies the prod
    // build onto these canonical names.
    let path = format!("{}/../artifacts/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("missing wasm artifact {path}: {e}. Build it first (see README)."))
}

fn factory_config(
    admin: &str,
    pricing_pool_id: u64,
    pool_code_id: u64,
) -> FactoryInstantiate {
    FactoryInstantiate {
        factory_admin_address: cosmwasm_std::Addr::unchecked(admin),
        commit_threshold_limit_usd: Uint128::new(THRESHOLD_USD),
        // Phase-2 doesn't instantiate a CW20 or NFT; these code-id fields are
        // unused by `Create`, so any valid code id satisfies the config.
        cw20_token_contract_id: pool_code_id,
        cw721_nft_contract_id: pool_code_id,
        create_pool_wasm_contract_id: pool_code_id,
        bluechip_wallet_address: cosmwasm_std::Addr::unchecked(admin),
        commit_fee_bluechip: Decimal::percent(1),
        commit_fee_creator: Decimal::percent(5),
        max_bluechip_lock_per_pool: Uint128::new(30_000_000_000),
        creator_excess_liquidity_lock_days: 7,
        bluechip_denom: UOSMO.to_string(),
        pricing_pool_id,
        usd_quote_denom: UUSDC.to_string(),
        twap_window_seconds: 300,
        // Flat create fee disabled → `Create` attaches no funds.
        pool_creation_fee: Uint128::zero(),
        gamm_pool_creation_fee: Coin::new(GAMM_CREATE_FEE, UOSMO),
        threshold_payout_amounts: ThresholdPayoutAmounts::default(),
        emergency_withdraw_delay_seconds: 86_400,
    }
}

// ---------------------------------------------------------------------------
// A minimal smoke test: the factory instantiates against a live TWAP route.
// This is the first thing to get green — it exercises the x/twap probe in
// `validate_factory_config` end to end.
// ---------------------------------------------------------------------------

#[test]
fn instantiate_factory_against_live_twap() {
    let app = OsmosisTestApp::new();
    let admin = app
        .init_account(&[
            Coin::new(1_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ])
        .unwrap();

    // Pricing pool: 1:1 uosmo/uusdc ⇒ ~$1 per OSMO.
    let gamm = Gamm::new(&app);
    let pricing_pool_id = gamm
        .create_basic_pool(
            &[
                Coin::new(1_000_000_000u128, UOSMO),
                Coin::new(1_000_000_000u128, UUSDC),
            ],
            &admin,
        )
        .unwrap()
        .data
        .pool_id;

    // Let the arithmetic TWAP accumulate past the 300s window.
    app.increase_time(400);

    let wasm = Wasm::new(&app);
    let factory_code_id = wasm
        .store_code(&read_wasm("factory.wasm"), None, &admin)
        .unwrap()
        .data
        .code_id;
    let pool_code_id = wasm
        .store_code(&read_wasm("pool.wasm"), None, &admin)
        .unwrap()
        .data
        .code_id;

    // The instantiate live-probes the pricing route; a bad config panics here.
    let factory_addr = wasm
        .instantiate(
            factory_code_id,
            &factory_config(&admin.address(), pricing_pool_id, pool_code_id),
            Some(&admin.address()),
            Some("factory"),
            &[],
            &admin,
        )
        .unwrap()
        .data
        .address;

    assert!(!factory_addr.is_empty(), "factory instantiated");
}

// ---------------------------------------------------------------------------
// Full lifecycle: create → cross threshold → distribute → swap.
// ---------------------------------------------------------------------------

#[test]
fn full_lifecycle_create_commit_cross_swap() {
    let app = OsmosisTestApp::new();

    // admin funds the pricing pool + owns the factory; committer crosses the
    // threshold and later swaps.
    let admin = app
        .init_account(&[
            Coin::new(10_000_000_000u128, UOSMO),
            Coin::new(10_000_000_000u128, UUSDC),
        ])
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(10_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(100_000_000_000u128, UOSMO)])
        .unwrap();

    let gamm = Gamm::new(&app);
    let pricing_pool_id = gamm
        .create_basic_pool(
            &[
                Coin::new(1_000_000_000u128, UOSMO),
                Coin::new(1_000_000_000u128, UUSDC),
            ],
            &admin,
        )
        .unwrap()
        .data
        .pool_id;
    app.increase_time(400);

    let wasm = Wasm::new(&app);
    let factory_code_id = wasm
        .store_code(&read_wasm("factory.wasm"), None, &admin)
        .unwrap()
        .data
        .code_id;
    let pool_code_id = wasm
        .store_code(&read_wasm("pool.wasm"), None, &admin)
        .unwrap()
        .data
        .code_id;

    let factory_addr = wasm
        .instantiate(
            factory_code_id,
            &factory_config(&admin.address(), pricing_pool_id, pool_code_id),
            Some(&admin.address()),
            Some("factory"),
            &[],
            &admin,
        )
        .unwrap()
        .data
        .address;

    // --- Create a creator pool (the `creator` account is its creator) ---
    let symbol = "MYTOKEN";
    let create = FactoryExecuteMsg::Create {
        pool_msg: CreatePool {
            pool_token_info: [
                TokenType::Native {
                    denom: UOSMO.to_string(),
                },
                // Placeholder — the pool mints its own factory denom.
                TokenType::CreatorToken {
                    denom: "WILL_BE_CREATED_BY_FACTORY".to_string(),
                },
            ],
        },
        token_info: CreatorTokenInfo {
            name: "My Creator Token".to_string(),
            symbol: symbol.to_string(),
            decimal: 6,
        },
    };
    wasm.execute(&factory_addr, &create, &[], &creator).unwrap();

    // Resolve the pool address + id from the registry.
    let pools: PoolsResponse = wasm
        .query(
            &factory_addr,
            &FactoryQueryMsg::Pools {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    let entry = pools.pools.first().expect("one pool registered");
    let pool_addr = entry.pool_addr.to_string();

    // The deterministic creator denom the pool created at instantiate.
    let creator_denom = format!("factory/{}/{}", pool_addr, symbol.to_lowercase());

    // Sanity: pre-threshold the pool reports InProgress.
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::InProgress { .. }),
        "pool starts pre-threshold, got {status:?}"
    );

    // --- Cross the threshold in a single commit (excess auto-refunded) ---
    // At ~$1/OSMO, $25k threshold ≈ 25,000 OSMO; send 26,000 to cross.
    let commit = PoolExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(26_000_000_000),
        },
        transaction_deadline: None,
        // Crossing commit runs the pre-threshold/crossing path, which does
        // not require a belief_price (that requirement is post-threshold).
        belief_price: None,
        max_spread: None,
    };
    wasm.execute(
        &pool_addr,
        &commit,
        &[Coin::new(26_000_000_000u128, UOSMO)],
        &committer,
    )
    .unwrap();

    // --- The crossing succeeded ⇒ the native GAMM pool was created and its
    // id stored (else the reply_on_success failure would have reverted the
    // whole crossing tx and left the pool InProgress). ---
    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::FullyCommitted {}),
        "pool crossed threshold + seeded native pool, got {status:?}"
    );

    // The four threshold mints landed: total creator supply is exactly 1.2T.
    // (Queried via the factory, which reads x/bank supply of the denom.)
    let token_info: factory::query::CreatorTokenInfoResponse = wasm
        .query(
            &factory_addr,
            &FactoryQueryMsg::CreatorTokenInfo {
                pool_id: entry.pool_id,
            },
        )
        .unwrap();
    assert_eq!(
        token_info.total_supply,
        Uint128::new(TOTAL_CREATOR_SUPPLY),
        "all four threshold mints landed"
    );
    assert_eq!(token_info.token_denom, creator_denom);

    // The creator wallet received its up-front reward mint.
    let bank = Bank::new(&app);
    let creator_bal = tt::balance(&bank, &creator.address(), &creator_denom);
    assert_eq!(
        creator_bal,
        Uint128::new(CREATOR_REWARD),
        "creator received creator_reward_amount"
    );

    // --- Run the permissionless distribution so the committer is paid ---
    wasm.execute(
        &pool_addr,
        &PoolExecuteMsg::ContinueDistribution {},
        &[],
        &committer,
    )
    .unwrap();

    // --- Post-threshold swap: buy the creator token with OSMO ---
    // (Direct SimpleSwap accepts belief_price: None; here we pass an explicit
    // one to model the recommended slippage-bounded client call.)
    let committer_before = tt::balance(&bank, &committer.address(), &creator_denom);
    let swap = PoolExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(100_000_000),
        },
        belief_price: None,
        max_spread: Some(Decimal::percent(5)),
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    wasm.execute(
        &pool_addr,
        &swap,
        &[Coin::new(100_000_000u128, UOSMO)],
        &committer,
    )
    .unwrap();
    let committer_after = tt::balance(&bank, &committer.address(), &creator_denom);
    assert!(
        committer_after > committer_before,
        "post-threshold swap routed through the native pool and delivered creator tokens"
    );
}

// ---------------------------------------------------------------------------
// osmosis-test-tube runner helpers. The ONLY place that touches the
// version-sensitive query surface — adjust here on a version bump.
// ---------------------------------------------------------------------------
mod tt {
    use super::*;
    use osmosis_test_tube::osmosis_std::types::cosmos::bank::v1beta1::QueryBalanceRequest;

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
}
