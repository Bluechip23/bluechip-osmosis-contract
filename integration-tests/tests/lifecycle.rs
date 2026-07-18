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

use creator_pool::msg::{
    CommitStatus, ExecuteMsg as PoolExecuteMsg, NativePoolIdResponse, QueryMsg as PoolQueryMsg,
};

use osmosis_test_tube::osmosis_std::types::cosmos::base::v1beta1::Coin as ProtoCoin;
use osmosis_test_tube::osmosis_std::types::osmosis::gamm::v1beta1::{
    MsgExitPool, MsgExitPoolResponse, MsgJoinPool, MsgJoinPoolResponse,
};
use osmosis_test_tube::osmosis_std::types::osmosis::poolmanager::v1beta1::{
    MsgSwapExactAmountIn, MsgSwapExactAmountInResponse, SwapAmountInRoute,
};
use osmosis_test_tube::{Account, Bank, Gamm, Module, OsmosisTestApp, Runner, Wasm};

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
/// Minted AT CROSSING: creator_reward (325B) + bluechip_reward (25B) +
/// pool_seed (350B) = 700B. The fourth split — commit_return (500B) — is
/// minted per-committer during distribution, so supply is 700B right after
/// crossing and reaches the full 1.2T only after `ContinueDistribution`.
const UPFRONT_MINTED_AT_CROSSING: u128 = 700_000_000_000;

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
    factory_config_with_gamm_fee(
        admin,
        pricing_pool_id,
        pool_code_id,
        Coin::new(GAMM_CREATE_FEE, UOSMO),
    )
}

fn factory_config_with_gamm_fee(
    admin: &str,
    pricing_pool_id: u64,
    pool_code_id: u64,
    gamm_pool_creation_fee: Coin,
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
    // threshold and later swaps. Accounts are funded generously: storing the
    // two large wasms alone costs ~27k OSMO in auto-estimated fees on the
    // embedded chain (fee = gas_limit * min_gas_price), so a 10k-OSMO admin
    // runs out at `store_code`.
    let admin = app
        .init_account(&[
            Coin::new(1_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ])
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
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

    // Only the THREE up-front splits mint at crossing (creator_reward +
    // bluechip_reward + pool_seed = 700B). The fourth split (commit_return,
    // 500B) is minted per-committer during distribution, so total supply is
    // 700B here and only reaches 1.2T after ContinueDistribution (asserted
    // below). (Queried via the factory, which reads x/bank supply of the denom.)
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
        Uint128::new(UPFRONT_MINTED_AT_CROSSING),
        "three up-front threshold mints landed (700B); commit_return mints during distribution"
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
    // Drain every batch (a single committer completes in one, but loop so the
    // test is agnostic to batch sizing).
    loop {
        wasm.execute(
            &pool_addr,
            &PoolExecuteMsg::ContinueDistribution {},
            &[],
            &committer,
        )
        .unwrap();
        let ds: Option<creator_pool::msg::DistributionStateResponse> = wasm
            .query(&pool_addr, &PoolQueryMsg::DistributionState {})
            .unwrap();
        match ds {
            Some(s) if s.is_distributing => {
                // ContinueDistribution has its own 5s rate limit between calls.
                app.increase_time(6);
                continue;
            }
            _ => break,
        }
    }

    // After distribution the fourth split (commit_return, 500B) has been
    // minted per-committer, so total supply is now the full 1.2T — all four
    // threshold mints have landed.
    let token_info_final: factory::query::CreatorTokenInfoResponse = wasm
        .query(
            &factory_addr,
            &FactoryQueryMsg::CreatorTokenInfo {
                pool_id: entry.pool_id,
            },
        )
        .unwrap();
    assert_eq!(
        token_info_final.total_supply,
        Uint128::new(TOTAL_CREATOR_SUPPLY),
        "all four threshold mints landed after distribution (1.2T)"
    );

    // --- Post-threshold swap: buy the creator token with OSMO ---
    // The committer just crossed (a Commit) and swaps share the per-wallet
    // 13s rate limit, so advance chain time past the window first.
    app.increase_time(30);
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
// Third-party liquidity on the native GAMM pool: add (MsgJoinPool) then
// remove (MsgExitPool).
//
// This is the exact path that replaced the retired internal-AMM + position-NFT
// LP model: after crossing, third-party liquidity lives on the *native* GAMM
// pool, and providers add/remove by messaging the gamm module directly — the
// creator-pool contract is NOT in the LP path (it only exposes NativePoolId so
// clients can find the pool). This test proves an external LP can:
//   1. discover the seeded pool via the contract's NativePoolId query,
//   2. buy some creator token through the contract (SimpleSwap),
//   3. two-sided-join the native pool (MsgJoinPool) and receive gamm shares,
//   4. exit (MsgExitPool) and get both assets back, shares fully burned.
// ---------------------------------------------------------------------------
#[test]
fn third_party_lp_join_and_exit_native_pool() {
    let app = OsmosisTestApp::new();

    let admin = app
        .init_account(&[
            Coin::new(1_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ])
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    // A plain third-party liquidity provider — not the creator, not the crosser.
    let lp = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
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

    // Create the creator pool.
    let symbol = "MYTOKEN";
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
            name: "My Creator Token".to_string(),
            symbol: symbol.to_string(),
            decimal: 6,
        },
    };
    wasm.execute(&factory_addr, &create, &[], &creator).unwrap();

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
    let creator_denom = format!("factory/{}/{}", pool_addr, symbol.to_lowercase());

    // Cross the threshold so the native GAMM pool is seeded.
    let commit = PoolExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: UOSMO.to_string(),
            },
            amount: Uint128::new(26_000_000_000),
        },
        transaction_deadline: None,
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

    // Discover the native pool via the contract query (the client's real path).
    let native: NativePoolIdResponse = wasm
        .query(&pool_addr, &PoolQueryMsg::NativePoolId {})
        .unwrap();
    let pool_id = native
        .pool_id
        .expect("native GAMM pool seeded after crossing");
    let lp_denom = native
        .lp_share_denom
        .clone()
        .unwrap_or_else(|| format!("gamm/pool/{}", pool_id));
    assert_eq!(lp_denom, format!("gamm/pool/{}", pool_id));

    let bank = Bank::new(&app);

    // The LP acquires the creator token via a DIRECT native-pool swap
    // (poolmanager MsgSwapExactAmountIn), so it holds both sides. Going direct
    // rather than through the contract's SimpleSwap keeps this test focused on
    // the LP join/exit path — the contract swap path (with its max_spread cap)
    // is covered by `full_lifecycle_create_commit_cross_swap`.
    let buy = MsgSwapExactAmountIn {
        sender: lp.address(),
        routes: vec![SwapAmountInRoute {
            pool_id,
            token_out_denom: creator_denom.clone(),
        }],
        token_in: Some(ProtoCoin {
            denom: UOSMO.to_string(),
            amount: "1000000000".to_string(),
        }),
        token_out_min_amount: "1".to_string(),
    };
    app.execute::<_, MsgSwapExactAmountInResponse>(buy, MsgSwapExactAmountIn::TYPE_URL, &lp)
        .unwrap();

    let lp_osmo = tt::balance(&bank, &lp.address(), UOSMO);
    let lp_creator = tt::balance(&bank, &lp.address(), &creator_denom);
    assert!(
        !lp_creator.is_zero(),
        "LP acquired creator token to provide the second side"
    );

    // Size a 1%-of-pool two-sided join. GAMM mints a fixed 1e20 shares at
    // creation; request 1% and cap each leg at the LP's full balance so the
    // module pulls exactly the proportional amount and never trips the cap.
    let total_shares: u128 = gamm
        .query_pool(pool_id)
        .unwrap()
        .total_shares
        .expect("pool has LP shares")
        .amount
        .parse()
        .unwrap();
    let share_out = Uint128::new(total_shares / 100);

    let lp_shares_before = tt::balance(&bank, &lp.address(), &lp_denom);
    assert!(lp_shares_before.is_zero(), "LP holds no shares pre-join");

    // Cosmos requires coins sorted by denom; sort explicitly rather than
    // relying on the denom-name ordering.
    let mut token_in_maxs = vec![
        ProtoCoin {
            denom: UOSMO.to_string(),
            amount: lp_osmo.to_string(),
        },
        ProtoCoin {
            denom: creator_denom.clone(),
            amount: lp_creator.to_string(),
        },
    ];
    token_in_maxs.sort_by(|a, b| a.denom.cmp(&b.denom));
    let join = MsgJoinPool {
        sender: lp.address(),
        pool_id,
        share_out_amount: share_out.to_string(),
        token_in_maxs,
    };
    app.execute::<_, MsgJoinPoolResponse>(join, MsgJoinPool::TYPE_URL, &lp)
        .unwrap();

    let lp_shares_after = tt::balance(&bank, &lp.address(), &lp_denom);
    assert_eq!(
        lp_shares_after, share_out,
        "MsgJoinPool minted exactly the requested gamm shares to the LP"
    );
    assert!(
        tt::balance(&bank, &lp.address(), UOSMO) < lp_osmo,
        "LP spent OSMO into the pool"
    );
    assert!(
        tt::balance(&bank, &lp.address(), &creator_denom) < lp_creator,
        "LP spent creator token into the pool"
    );

    // --- Remove all liquidity: burn the shares, receive both assets back. ---
    // OSMO is also the gas denom, so the LP's raw OSMO balance is confounded by
    // the (non-trivial, on this chain) tx fee. Prove the OSMO return via the
    // pool's OSMO reserve dropping instead; use the (non-gas) creator token for
    // the LP-balance-side proof.
    let creator_before_exit = tt::balance(&bank, &lp.address(), &creator_denom);
    let reserves_before_exit = gamm.query_pool_reserves(pool_id).unwrap();
    let exit = MsgExitPool {
        sender: lp.address(),
        pool_id,
        share_in_amount: lp_shares_after.to_string(),
        token_out_mins: vec![],
    };
    app.execute::<_, MsgExitPoolResponse>(exit, MsgExitPool::TYPE_URL, &lp)
        .unwrap();
    let reserves_after_exit = gamm.query_pool_reserves(pool_id).unwrap();

    let reserve_of = |rs: &[Coin], d: &str| -> u128 {
        rs.iter().find(|c| c.denom == d).map(|c| c.amount.u128()).unwrap_or(0)
    };

    assert!(
        tt::balance(&bank, &lp.address(), &lp_denom).is_zero(),
        "all LP shares burned on exit"
    );
    assert!(
        tt::balance(&bank, &lp.address(), &creator_denom) > creator_before_exit,
        "exit returned creator token to the LP"
    );
    assert!(
        reserve_of(&reserves_after_exit, UOSMO) < reserve_of(&reserves_before_exit, UOSMO),
        "exit released OSMO from the pool"
    );
    assert!(
        reserve_of(&reserves_after_exit, &creator_denom)
            < reserve_of(&reserves_before_exit, &creator_denom),
        "exit released creator token from the pool"
    );
}

// ---------------------------------------------------------------------------
// Cross-denom GAMM creation fee: reproduce the osmosis-1 mainnet scenario.
//
// osmosis-1's `x/poolmanager` pool-creation fee is denominated in Noble
// USDC (20 USDC as of 2026-07), NOT uosmo. The pool contract only ever
// holds uosmo (commits) + its own creator denom, so the crossing must
// convert its retained 1%-commit-fee reserve into the exact fee coin via a
// MsgSwapExactAmountOut through the pricing pool BEFORE
// MsgCreateBalancerPool executes — funded by protocol revenue, never the
// creator. This test rewrites the chain's poolmanager params so the fee is
// `20 uusdc` (exactly the mainnet shape against this test's uosmo/uusdc
// pricing pool) and proves the whole crossing chain — reserve → exact-out
// swap → create → charge — lands atomically on a real chain.
// ---------------------------------------------------------------------------
#[test]
fn cross_denom_usdc_fee_crossing_swaps_and_creates_pool() {
    use osmosis_test_tube::cosmrs::Any;
    use osmosis_test_tube::osmosis_std::types::osmosis::poolmanager::v1beta1::Params as PmParams;
    use prost::Message;

    let app = OsmosisTestApp::new();

    let admin = app
        .init_account(&[
            Coin::new(1_000_000_000_000u128, UOSMO),
            Coin::new(1_000_000_000_000u128, UUSDC),
        ])
        .unwrap();
    let creator = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
        .unwrap();
    let committer = app
        .init_account(&[Coin::new(1_000_000_000_000u128, UOSMO)])
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

    // --- Rewrite poolmanager params: fee = 20 USDC (the mainnet shape). ---
    // Read-modify-write so the taker-fee config and authorized quote denoms
    // stay intact; only the creation fee changes.
    let usdc_fee = 20_000_000u128; // $20, 6 decimals
    let mut pm_params: PmParams = app
        .get_param_set("poolmanager", PmParams::TYPE_URL)
        .expect("read poolmanager params");
    pm_params.pool_creation_fee =
        vec![osmosis_test_tube::osmosis_std::types::cosmos::base::v1beta1::Coin {
            denom: UUSDC.to_string(),
            amount: usdc_fee.to_string(),
        }];
    app.set_param_set(
        "poolmanager",
        Any {
            type_url: PmParams::TYPE_URL.to_string(),
            value: pm_params.encode_to_vec(),
        },
    )
    .expect("set poolmanager params");

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

    // Factory config mirrors the chain: gamm fee = 20 uusdc (the USD quote
    // denom), so validate_factory_config accepts it and the CommitContext
    // response carries it to the pool.
    let factory_addr = wasm
        .instantiate(
            factory_code_id,
            &factory_config_with_gamm_fee(
                &admin.address(),
                pricing_pool_id,
                pool_code_id,
                Coin::new(usdc_fee, UUSDC),
            ),
            Some(&admin.address()),
            Some("factory"),
            &[],
            &admin,
        )
        .unwrap()
        .data
        .address;

    // Create the creator pool + cross the threshold in one commit. The
    // crossing is atomic: 1% retention → MsgSwapExactAmountOut (uosmo →
    // 20 uusdc via the pricing pool) → MsgCreateBalancerPool (module
    // charges the 20 uusdc) → mints/distribution. Any failing leg reverts
    // the whole tx, so FullyCommitted afterwards proves the entire chain.
    let symbol = "XDENOM";
    wasm.execute(
        &factory_addr,
        &FactoryExecuteMsg::Create {
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
                name: "Cross Denom Fee".to_string(),
                symbol: symbol.to_string(),
                decimal: 6,
            },
        },
        &[],
        &creator,
    )
    .unwrap();
    let pools: PoolsResponse = wasm
        .query(
            &factory_addr,
            &FactoryQueryMsg::Pools {
                start_after: None,
                limit: None,
            },
        )
        .unwrap();
    let pool_addr = pools.pools.first().expect("pool registered").pool_addr.to_string();

    wasm.execute(
        &pool_addr,
        &PoolExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: UOSMO.to_string(),
                },
                amount: Uint128::new(26_000_000_000),
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
        &[Coin::new(26_000_000_000u128, UOSMO)],
        &committer,
    )
    .expect("crossing must swap uosmo->uusdc for the fee and create the native pool");

    let status: CommitStatus = wasm
        .query(&pool_addr, &PoolQueryMsg::IsFullyCommited {})
        .unwrap();
    assert!(
        matches!(status, CommitStatus::FullyCommitted {}),
        "crossing paid the USDC-denominated creation fee and seeded the pool, got {status:?}"
    );

    // The native pool exists and is discoverable — the create actually ran
    // (the module would have rejected it without the 20 uusdc in balance).
    let native: creator_pool::msg::NativePoolIdResponse = wasm
        .query(&pool_addr, &PoolQueryMsg::NativePoolId {})
        .unwrap();
    assert!(native.pool_id.is_some(), "native GAMM pool id stored");

    // No USDC may be left stranded in the pool: exact-out acquired exactly
    // the fee and the module charged exactly the fee.
    let bank = Bank::new(&app);
    let pool_usdc = tt::balance(&bank, &pool_addr, UUSDC);
    assert!(
        pool_usdc.is_zero(),
        "exact-out swap must leave no USDC dust (got {pool_usdc})"
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
