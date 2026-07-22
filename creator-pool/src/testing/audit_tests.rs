//! Independent adversarial-audit regression tests.
//!
//! Each test here is written to FAIL if a specific bug were present and to
//! PASS on the current (believed-correct) code, closing a coverage gap the
//! existing suite left open. They are named after the attack / invariant
//! they defend so a reviewer reading the file understands what is being
//! constrained, not merely which function is exercised.
//!
//! Harness note: like the rest of the creator-pool suite these are unit
//! tests over `mock_dependencies` with the Osmosis modules (gamm /
//! poolmanager / tokenfactory / twap) mocked. They therefore assert on the
//! MESSAGES / STATE the contract produces, not on live chain execution or
//! CosmWasm's real revert-on-`Err` (MockStorage does not roll back). The
//! places where that boundary matters are called out inline and in the
//! findings report.

use std::collections::BTreeMap;

use cosmwasm_std::testing::{message_info, mock_dependencies, mock_env};
use cosmwasm_std::{
    Addr, BankMsg, Binary, Coin, CosmosMsg, Order, Reply, SubMsgResponse, SubMsgResult, Uint128,
};
use prost::Message;

use crate::asset::{TokenInfo, TokenType};
use crate::contract::{execute, reply};
use crate::error::ContractError;
use crate::generic_helpers::process_distribution_batch;
use crate::msg::ExecuteMsg;
use crate::state::{
    DistributionState, COMMITFEEINFO, COMMIT_LEDGER, DISTRIBUTION_STATE, IS_THRESHOLD_HIT,
    PENDING_MINT_REPLIES, POOL_ID, POOL_INFO, REPLY_ID_CREATE_POOL,
    REPLY_ID_FACTORY_NOTIFY_INITIAL, USD_RAISED_FROM_COMMIT,
};
use crate::testing::fixtures::{
    mock_dependencies_with_balance, setup_pool_post_threshold, setup_pool_storage,
    with_factory_oracle,
};

// ===========================================================================
// F-check: distribution math — pro-rata conservation, no over-claim, dust
//          settles to the creator. Whale + dust extremes, run to completion.
// ===========================================================================
//
// Attack defended: a rounding-UP bug, or any path where the sum of
// per-committer allocations EXCEEDS `total_to_distribute` (which would let
// the last claimant either over-claim or find the mint under-funded), or the
// dust being misattributed. `calculate_committer_reward` floors each share
// via a Uint256 intermediate; the leftover must go to the creator and the
// grand total must equal `total_to_distribute` EXACTLY.
#[test]
fn distribution_conserves_supply_across_whale_and_dust_committers() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    // Indivisible numbers so flooring produces genuine dust (unlike the
    // fixture's evenly-divisible defaults). A whale (usd=3) alongside three
    // dust committers (usd=1) — the "both extremes" case.
    const TOTAL_TO_DISTRIBUTE: u128 = 1_000_000_007;
    const TOTAL_COMMITTED_USD: u128 = 6;
    let committers: [(&str, u128); 4] = [
        ("aaa_dust", 1),
        ("bbb_dust", 1),
        ("ccc_dust", 1),
        ("whale", 3),
    ];
    // Invariant the crossing guarantees and distribution relies on:
    // sum(ledger) == total_committed_usd.
    assert_eq!(
        committers.iter().map(|(_, u)| *u).sum::<u128>(),
        TOTAL_COMMITTED_USD,
        "test setup: ledger USD must sum to total_committed_usd"
    );

    for (name, usd) in committers.iter() {
        COMMIT_LEDGER
            .save(&mut deps.storage, &Addr::unchecked(*name), &Uint128::new(*usd))
            .unwrap();
    }

    let env = mock_env();
    // Small gas budget → batch size 2, so the payout spans multiple batches
    // and the dust settlement fires only on the true final batch.
    DISTRIBUTION_STATE
        .save(
            &mut deps.storage,
            &DistributionState {
                is_distributing: true,
                total_to_distribute: Uint128::new(TOTAL_TO_DISTRIBUTE),
                total_committed_usd: Uint128::new(TOTAL_COMMITTED_USD),
                last_processed_key: None,
                distributions_remaining: committers.len() as u32,
                estimated_gas_per_distribution: 50,
                max_gas_per_tx: 100,
                last_successful_batch_size: None,
                consecutive_failures: 0,
                started_at: env.block.time,
                last_updated: env.block.time,
                distributed_so_far: Uint128::zero(),
            },
        )
        .unwrap();

    let pool_info = POOL_INFO.load(&deps.storage).unwrap();
    let creator = COMMITFEEINFO
        .load(&deps.storage)
        .unwrap()
        .creator_wallet_address;

    // Drive every batch to completion.
    let mut guard = 0;
    loop {
        let (_subs, _n) = process_distribution_batch(&mut deps.storage, &pool_info, &env).unwrap();
        if DISTRIBUTION_STATE.may_load(&deps.storage).unwrap().is_none() {
            break;
        }
        guard += 1;
        assert!(guard < 50, "distribution did not terminate");
    }

    // Every dispatched mint left a PendingMint stash (we never drove the
    // replies, so they persist) — read them as the authoritative per-user
    // minted amount.
    let mut minted: BTreeMap<String, u128> = BTreeMap::new();
    for entry in PENDING_MINT_REPLIES.range(&deps.storage, None, None, Order::Ascending) {
        let (_id, pm) = entry.unwrap();
        *minted.entry(pm.user.to_string()).or_default() += pm.amount.u128();
    }

    // Independent floor reimplementation (NOT the contract's Uint256 path) so
    // a rounding-direction regression is actually caught.
    let expected_share =
        |usd: u128| -> u128 { (usd * TOTAL_TO_DISTRIBUTE) / TOTAL_COMMITTED_USD };

    let mut committer_sum = 0u128;
    for (name, usd) in committers.iter() {
        let got = *minted.get(*name).unwrap_or(&0);
        let want = expected_share(*usd);
        assert_eq!(
            got, want,
            "committer {name} must receive exactly floor(usd*total/committed)={want}, got {got} \
             (an over-claim or rounding-up bug would diverge here)"
        );
        committer_sum += got;
    }

    // Dust = the flooring residual, and it must land on the CREATOR.
    let creator_got = *minted.get(&creator.to_string()).unwrap_or(&0);
    let expected_dust = TOTAL_TO_DISTRIBUTE - committer_sum;
    assert_eq!(
        creator_got, expected_dust,
        "creator must absorb exactly the flooring dust ({expected_dust}), got {creator_got}"
    );
    assert!(expected_dust > 0, "test must exercise a non-zero dust residual");

    // The load-bearing safety invariant: total minted == supply. Never more
    // (would let someone claim beyond their share / brick the last claimant),
    // never less-than-accounted.
    let grand_total: u128 = minted.values().sum();
    assert_eq!(
        grand_total, TOTAL_TO_DISTRIBUTE,
        "sum of ALL allocations (committers + creator dust) must equal total_to_distribute exactly"
    );
}

// ===========================================================================
// F-check: the crossing records only the THRESHOLD PORTION for the crosser,
//          so sum(COMMIT_LEDGER) == commit_amount_for_threshold_usd.
// ===========================================================================
//
// Attack defended: if the crossing recorded the crosser's FULL commit_value
// (instead of `value_to_threshold`), sum(ledger) would exceed
// total_committed_usd, and distribution's pro-rata (sum of usd*total/committed)
// could then exceed `total_to_distribute` — an over-mint. Pin the invariant
// with a PRIOR committer present so the sum is non-trivial.
#[test]
fn overshoot_crossing_keeps_ledger_sum_equal_to_threshold() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    // rate 1e6 == $1 per native micro-unit (native micros == usd micros).
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));
    let env = mock_env();

    // Prior pre-threshold committer: $10k of the $25k target.
    let early_amt = Uint128::new(10_000_000_000);
    execute(
        deps.as_mut(),
        env.clone(),
        message_info(
            &Addr::unchecked("early_bird"),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: early_amt,
            }],
        ),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: early_amt,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap();

    // Whale commits $20k → crosses the $25k threshold with $5k of excess.
    // value_to_threshold is $15k; the crosser must be ledgered for $15k, not
    // the full $20k.
    let whale_amt = Uint128::new(20_000_000_000);
    let res = execute(
        deps.as_mut(),
        env.clone(),
        message_info(
            &Addr::unchecked("whale"),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: whale_amt,
            }],
        ),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: whale_amt,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap();
    assert_eq!(
        res.attributes
            .iter()
            .find(|a| a.key == "phase")
            .unwrap()
            .value,
        "threshold_crossing"
    );

    assert!(IS_THRESHOLD_HIT.load(&deps.storage).unwrap());
    let threshold = Uint128::new(25_000_000_000);
    assert_eq!(USD_RAISED_FROM_COMMIT.load(&deps.storage).unwrap(), threshold);

    // The crosser's ledger entry is exactly value_to_threshold ($15k), and
    // the WHOLE ledger sums to the threshold — the ceiling distribution can
    // ever allocate against.
    let whale_ledger = COMMIT_LEDGER
        .load(&deps.storage, &Addr::unchecked("whale"))
        .unwrap();
    assert_eq!(
        whale_ledger,
        Uint128::new(15_000_000_000),
        "crosser must be ledgered for value_to_threshold ($15k), not the full $20k commit"
    );
    let ledger_sum: Uint128 = COMMIT_LEDGER
        .range(&deps.storage, None, None, Order::Ascending)
        .map(|e| e.unwrap().1)
        .fold(Uint128::zero(), |acc, v| acc + v);
    assert_eq!(
        ledger_sum, threshold,
        "sum(COMMIT_LEDGER) must equal the threshold so pro-rata distribution can never over-allocate"
    );
}

// ===========================================================================
// F-check: overshoot crossing refunds the EXACT post-fee excess.
// ===========================================================================
//
// The existing overshoot test only asserts the refund is non-zero. Pin the
// exact amount so a regression in the split math (fee application or the
// threshold/excess partition) is caught.
#[test]
fn overshoot_crossing_refunds_exact_post_fee_excess_to_crosser() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(100_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    // Start $24,999 raised so a $5 commit crosses the $25k threshold with $4
    // of excess.
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(24_999_000_000))
        .unwrap();
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));
    let env = mock_env();

    let commit_amount = Uint128::new(5_000_000); // $5 gross
    let res = execute(
        deps.as_mut(),
        env,
        message_info(
            &Addr::unchecked("whale"),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: commit_amount,
            }],
        ),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: commit_amount,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap();

    // Hand-derivation:
    //   fees        = 1% + 5% of 5_000_000 = 50_000 + 250_000 = 300_000
    //   after_fees  = 4_700_000
    //   to_threshold(native) = usd_to_native($1) = 1_000_000
    //   threshold_after_fees = 4_700_000 * 1_000_000 / 5_000_000 = 940_000
    //   excess_after_fees    = 4_700_000 - 940_000            = 3_760_000
    const EXPECTED_REFUND: u128 = 3_760_000;

    let refund_attr = res
        .attributes
        .iter()
        .find(|a| a.key == "bluechip_excess_refunded")
        .unwrap()
        .value
        .clone();
    assert_eq!(
        refund_attr,
        EXPECTED_REFUND.to_string(),
        "refund attribute must equal the exact post-fee excess"
    );

    // And the actual BankMsg to the crosser must carry exactly that coin.
    let refund_coin = res
        .messages
        .iter()
        .find_map(|m| match &m.msg {
            CosmosMsg::Bank(BankMsg::Send { to_address, amount }) if to_address == "whale" => {
                Some(amount.clone())
            }
            _ => None,
        })
        .expect("a refund BankMsg::Send to the crosser must be present");
    assert_eq!(refund_coin.len(), 1);
    assert_eq!(refund_coin[0].denom, "ubluechip");
    assert_eq!(
        refund_coin[0].amount,
        Uint128::new(EXPECTED_REFUND),
        "the refunded coin amount must equal the exact post-fee excess, not an over/under-refund"
    );
}

// ===========================================================================
// F-check: crossing message ORDER — every seed/mint/fee/refund plain message
//          executes BEFORE MsgCreateBalancerPool, and the factory-notify
//          submessage is dispatched AFTER it.
// ===========================================================================
//
// The pool-seed creator tokens are minted to the contract by a plain
// (reply_never) message; MsgCreateBalancerPool then seeds them. If a refactor
// reordered the create-pool SubMsg ahead of the seed mint, the create would
// run before the pool holds its seed and brick the crossing. Osmosis modules
// aren't executed here, but the message ORDER the contract emits is exactly
// what determines on-chain sequencing, so this pins the invariant.
#[test]
fn crossing_dispatches_seed_mints_before_pool_creation_and_notify_last() {
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(100_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    USD_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(24_999_000_000))
        .unwrap();
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));
    let env = mock_env();

    let commit_amount = Uint128::new(5_000_000);
    let res = execute(
        deps.as_mut(),
        env,
        message_info(
            &Addr::unchecked("whale"),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: commit_amount,
            }],
        ),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: commit_amount,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap();

    let create_idx = res
        .messages
        .iter()
        .position(|m| m.id == REPLY_ID_CREATE_POOL)
        .expect("crossing must emit the MsgCreateBalancerPool SubMsg");
    let notify_idx = res
        .messages
        .iter()
        .position(|m| m.id == REPLY_ID_FACTORY_NOTIFY_INITIAL)
        .expect("crossing must emit the factory-notify SubMsg");

    assert!(
        notify_idx > create_idx,
        "factory-notify (reply_on_error) must be dispatched AFTER pool creation, got notify@{notify_idx} create@{create_idx}"
    );

    // Every plain (reply_never, id==0) message — the two fee sends, the three
    // split mints (incl. the pool-seed mint to the contract), and the excess
    // refund — must precede the create-pool SubMsg.
    for (i, m) in res.messages.iter().enumerate() {
        if m.id == 0 {
            assert!(
                i < create_idx,
                "plain message at index {i} (a mint/fee/refund) must execute before \
                 MsgCreateBalancerPool at {create_idx}"
            );
        }
    }

    // Sanity: the three split mints (non-Bank plain messages) really are among
    // the messages preceding creation — otherwise the pool would hold no seed.
    let mint_like_before_create = res.messages[..create_idx]
        .iter()
        .filter(|m| m.id == 0 && !matches!(m.msg, CosmosMsg::Bank(_)))
        .count();
    assert!(
        mint_like_before_create >= 3,
        "expected the 3 up-front split mints (incl. pool-seed) before pool creation, saw {mint_like_before_create}"
    );
}

// ===========================================================================
// F-check: the create-pool reply records POOL_ID from the decoded response,
//          which is what makes the pool swappable post-crossing.
// ===========================================================================
//
// This reply path (REPLY_ID_CREATE_POOL) was entirely untested. A regression
// in `parse_created_pool_id` (wrong type URL, decode, or the deprecated-data
// fallback) would silently leave POOL_ID unset, so every post-threshold swap
// would revert with ShortOfThreshold even though the threshold crossed.
#[test]
fn create_pool_reply_records_pool_id_from_response() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);
    assert!(POOL_ID.may_load(&deps.storage).unwrap().is_none());

    let encoded = osmosis_std::types::osmosis::gamm::poolmodels::balancer::v1beta1::MsgCreateBalancerPoolResponse {
        pool_id: 4242,
    }
    .encode_to_vec();

    #[allow(deprecated)]
    let reply_msg = Reply {
        id: REPLY_ID_CREATE_POOL,
        payload: Binary::default(),
        gas_used: 0,
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![],
            data: None,
            msg_responses: vec![cosmwasm_std::MsgResponse {
                type_url: "/osmosis.gamm.poolmodels.balancer.v1beta1.MsgCreateBalancerPoolResponse"
                    .to_string(),
                value: Binary::from(encoded),
            }],
        }),
    };

    reply(deps.as_mut(), mock_env(), reply_msg).unwrap();
    assert_eq!(
        POOL_ID.load(&deps.storage).unwrap(),
        4242,
        "the create-pool reply must persist the native pool id so swaps can route"
    );
}

// The create-pool reply must FAIL LOUDLY (not silently leave POOL_ID unset)
// if the response carries no MsgCreateBalancerPoolResponse. Because the SubMsg
// is reply_on_success, an error here reverts the whole crossing tx on-chain —
// the correct fail-closed behaviour.
#[test]
fn create_pool_reply_without_response_errors_rather_than_leaving_pool_id_unset() {
    let mut deps = mock_dependencies();
    setup_pool_storage(&mut deps);

    #[allow(deprecated)]
    let reply_msg = Reply {
        id: REPLY_ID_CREATE_POOL,
        payload: Binary::default(),
        gas_used: 0,
        result: SubMsgResult::Ok(SubMsgResponse {
            events: vec![],
            data: None,
            msg_responses: vec![],
        }),
    };

    let err = reply(deps.as_mut(), mock_env(), reply_msg).unwrap_err();
    assert!(
        err.to_string().contains("MsgCreateBalancerPoolResponse"),
        "a create-pool reply without the response must error, got: {err}"
    );
    assert!(
        POOL_ID.may_load(&deps.storage).unwrap().is_none(),
        "POOL_ID must remain unset when the reply cannot be parsed"
    );
}

// ===========================================================================
// F-1 (fixed): direct SimpleSwap now REQUIRES belief_price; only the
//              registered router (which enforces end-to-end minimum_receive)
//              is exempt.
// ===========================================================================
//
// Attack defended: a direct SimpleSwap caller who omits belief_price is
// protected only by the on-chain estimate floor, which is computed at
// already-front-run pool state and is NOT sandwich-resistant. The fix forces
// a direct caller to supply a belief_price (matching the commit path, H-3),
// while exempting the registered multi-hop router by address — the router
// bounds the whole route with minimum_receive, so its per-hop null-belief
// calls are safe.
#[test]
fn direct_simple_swap_requires_belief_price_but_registered_router_is_exempt() {
    use crate::mock_querier::mock_deps_estimate;

    let swap_amount = Uint128::new(100_000_000);
    let offer = |amt: Uint128| ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: amt,
        },
        belief_price: None,
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    let funds = |amt: Uint128| {
        vec![Coin {
            denom: "ubluechip".to_string(),
            amount: amt,
        }]
    };

    // (A) A DIRECT caller with belief_price = None is now REJECTED.
    let mut deps = mock_deps_estimate(&funds(Uint128::new(1_000_000_000)));
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&Addr::unchecked("direct_caller"), &funds(swap_amount)),
        offer(swap_amount),
    )
    .unwrap_err();
    assert!(
        matches!(err, ContractError::BeliefPriceRequired {}),
        "a direct SimpleSwap with no belief_price must be rejected (sandwich exposure); got {err:?}"
    );

    // (B) The REGISTERED ROUTER (per the mock querier's RegisteredRouter
    // response) is exempt — it swaps null-belief because it enforces an
    // end-to-end minimum_receive across the route.
    let mut deps = mock_deps_estimate(&funds(Uint128::new(1_000_000_000)));
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");
    let res = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&Addr::unchecked("registered_router"), &funds(swap_amount)),
        offer(swap_amount),
    )
    .expect("the registered router must be exempt from the belief_price requirement");
    assert_eq!(
        res.attributes
            .iter()
            .find(|a| a.key == "action")
            .unwrap()
            .value,
        "swap",
        "router-originated null-belief SimpleSwap should still dispatch"
    );

    // (C) A direct caller WITH an explicit belief_price is accepted.
    let mut deps = mock_deps_estimate(&funds(Uint128::new(1_000_000_000)));
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");
    let with_belief = ExecuteMsg::SimpleSwap {
        offer_asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: swap_amount,
        },
        // Loose belief price → estimate floor still binds; the point is that
        // a bound is PRESENT.
        belief_price: Some(cosmwasm_std::Decimal::percent(200)),
        max_spread: None,
        allow_high_max_spread: None,
        to: None,
        transaction_deadline: None,
    };
    let res = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&Addr::unchecked("direct_caller"), &funds(swap_amount)),
        with_belief,
    )
    .expect("a direct SimpleSwap that supplies a belief_price must be accepted");
    assert_eq!(
        res.attributes
            .iter()
            .find(|a| a.key == "action")
            .unwrap()
            .value,
        "swap"
    );

    // (D) The commit swap still requires belief_price too (unchanged, H-3).
    let mut deps = mock_deps_estimate(&funds(Uint128::new(1_000_000_000)));
    setup_pool_post_threshold(&mut deps);
    deps.querier
        .set_factory_oracle(Uint128::new(1_000_000), "bluechip_treasury");
    let err = execute(
        deps.as_mut(),
        mock_env(),
        message_info(&Addr::unchecked("committer"), &funds(swap_amount)),
        ExecuteMsg::Commit {
            asset: TokenInfo {
                info: TokenType::Native {
                    denom: "ubluechip".to_string(),
                },
                amount: swap_amount,
            },
            transaction_deadline: None,
            belief_price: None,
            max_spread: None,
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, ContractError::BeliefPriceRequired {}),
        "commit swap still requires belief_price; got {err:?}"
    );
}

// ===========================================================================
// F-3: pool-side sanity CEILING on the factory-delegated oracle rate.
// ===========================================================================
//
// The pool delegates its entire USD valuation to the factory. A factory bug,
// a mis-set pricing pool, or a wrong-decimals quote denom that slipped past
// the factory's own gate would otherwise let an absurd rate value a dust
// commit as a fortune and cross the threshold. The pool now rejects any rate
// above POOL_RATE_MAX ($10,000/native). This test asserts the SAME commit is
// rejected above the ceiling and accepted at a normal rate — so the rejection
// is the ceiling, not an unrelated failure.
#[test]
fn commit_rejects_oracle_rate_above_pool_ceiling() {
    let commit_amount = Uint128::new(5_000_000);
    let msg = || ExecuteMsg::Commit {
        asset: TokenInfo {
            info: TokenType::Native {
                denom: "ubluechip".to_string(),
            },
            amount: commit_amount,
        },
        transaction_deadline: None,
        belief_price: None,
        max_spread: None,
    };
    let info = || {
        message_info(
            &Addr::unchecked("committer"),
            &[Coin {
                denom: "ubluechip".to_string(),
                amount: commit_amount,
            }],
        )
    };

    // Above the ceiling ($10,000/native == rate 10_000_000_000; use +1) →
    // rejected as an invalid oracle price BEFORE any funds are banked.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(10_000 * 1_000_000 + 1));
    let err = execute(deps.as_mut(), mock_env(), info(), msg()).unwrap_err();
    assert!(
        matches!(err, ContractError::InvalidOraclePrice {}),
        "a rate above the pool ceiling must be rejected; got {err:?}"
    );
    assert!(
        COMMIT_LEDGER
            .may_load(&deps.storage, &Addr::unchecked("committer"))
            .unwrap()
            .is_none(),
        "no ledger entry may be written when the oracle rate is rejected"
    );

    // Same commit at a normal $1 rate → accepted (pre-threshold funding),
    // proving the rejection above is the ceiling and nothing else.
    let mut deps = mock_dependencies_with_balance(&[Coin {
        denom: "ubluechip".to_string(),
        amount: Uint128::new(1_000_000_000),
    }]);
    setup_pool_storage(&mut deps);
    with_factory_oracle(&mut deps, Uint128::new(1_000_000));
    execute(deps.as_mut(), mock_env(), info(), msg()).expect("a normal-rate commit must succeed");
    assert!(
        COMMIT_LEDGER
            .may_load(&deps.storage, &Addr::unchecked("committer"))
            .unwrap()
            .is_some(),
        "a normal-rate commit must record the committer"
    );
}
