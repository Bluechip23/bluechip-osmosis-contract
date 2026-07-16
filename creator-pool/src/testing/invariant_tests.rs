//! Property + edge-case tests for the threshold-crossing FUND-CONSERVATION
//! invariant.
//!
//! At a crossing the pool's entire OSMO balance is `raised_net + reserved`
//! (net commits plus the bluechip fee retained toward the gamm creation
//! fee). `trigger_threshold_payout` splits that balance four ways:
//!   - `seed_osmo`  seeded into the native GAMM pool,
//!   - `creation_fee` auto-charged by the gamm module,
//!   - `leftover`   (`reserved - creation_fee`) remitted to the wallet,
//!   - `earmark`    (`raised_net - max_lock`, over-cap only) left in the pool
//!                  for the creator's time-locked claim.
//!
//! The load-bearing invariant is that these EXACTLY account for the balance
//! with nothing created or destroyed:
//!
//!   seed_osmo + creation_fee + leftover + earmark == raised_net + reserved
//!
//! and that the creator earmark equals the true over-cap excess
//! (`raised_net - max_lock`), never reduced by the creation-fee shortfall
//! the protocol absorbs. These are the same relations traced by hand in the
//! audit; the property test pins them across a wide random input space so a
//! future refactor of the seed/reserve math can't silently break them.
//!
//! The mock querier answers the `x/poolmanager` params query with "not
//! found", so `query_pool_creation_fee` falls back to the configured value
//! and `creation_fee == configured` throughout — the on-chain live-fee
//! branch is an integration-test concern (see `integration-tests/`).

use cosmwasm_std::{BankMsg, CosmosMsg, Decimal, QuerierWrapper, Uint128};
use proptest::prelude::*;

use crate::error::ContractError;
use crate::generic_helpers::trigger_threshold_payout;
use crate::mock_querier::mock_deps_estimate;
use crate::state::{
    CommitLimitInfo, BLUECHIP_FEE_RESERVED, COMMITFEEINFO, COMMIT_LIMIT_INFO,
    CREATION_FEE_RESERVE_TARGET, CREATOR_EXCESS_POSITION, IS_THRESHOLD_HIT,
    NATIVE_RAISED_FROM_COMMIT, POOL_INFO, SEED_LIQUIDITY, THRESHOLD_PAYOUT_AMOUNTS,
};
use crate::testing::fixtures::setup_pool_storage;

/// Canonical pool-seed creator-token amount from the fixed payout split.
const POOL_SEED: u128 = 350_000_000_000;

/// Drive one crossing with the given (raised_net, max_lock, configured_fee,
/// reserved) and return the observable amounts, or the seed-zero error.
#[derive(Debug)]
struct CrossingOutcome {
    seed_osmo: Uint128,
    seed_creator: Uint128,
    creation_fee: Uint128,
    leftover: Uint128,
    earmark_bluechip: Uint128,
    earmark_token: Uint128,
    reserved_after: Uint128,
    threshold_hit_after: bool,
}

fn run_crossing(
    raised_net: u128,
    max_lock: u128,
    configured_fee: u128,
    reserved: u128,
) -> Result<CrossingOutcome, ContractError> {
    let mut deps = mock_deps_estimate(&[]);
    setup_pool_storage(&mut deps);

    // Pre-crossing state under test.
    NATIVE_RAISED_FROM_COMMIT
        .save(&mut deps.storage, &Uint128::new(raised_net))
        .unwrap();
    BLUECHIP_FEE_RESERVED
        .save(&mut deps.storage, &Uint128::new(reserved))
        .unwrap();
    CREATION_FEE_RESERVE_TARGET
        .save(&mut deps.storage, &Uint128::new(configured_fee))
        .unwrap();

    let pool_info = POOL_INFO.load(&deps.storage).unwrap();
    let payout = THRESHOLD_PAYOUT_AMOUNTS.load(&deps.storage).unwrap();
    let fee_info = COMMITFEEINFO.load(&deps.storage).unwrap();
    // Vary the per-pool bluechip cap via the passed-in config (trigger reads
    // the cap from this param, not from storage).
    let mut commit_config: CommitLimitInfo = COMMIT_LIMIT_INFO.load(&deps.storage).unwrap();
    commit_config.max_bluechip_lock_per_pool = Uint128::new(max_lock);

    let env = cosmwasm_std::testing::mock_env();
    let msgs = trigger_threshold_payout(
        &mut deps.storage,
        &QuerierWrapper::new(&deps.querier),
        &pool_info,
        &commit_config,
        &payout,
        &fee_info,
        &fee_info.bluechip_wallet_address,
        Decimal::permille(3),
        &env,
    )?;

    let (seed_osmo, seed_creator) = SEED_LIQUIDITY.load(&deps.storage).unwrap();
    let leftover = match msgs.reserve_remit {
        Some(CosmosMsg::Bank(BankMsg::Send { amount, .. })) => amount
            .iter()
            .find(|c| c.denom == "ubluechip")
            .map(|c| c.amount)
            .unwrap_or_default(),
        _ => Uint128::zero(),
    };
    let (earmark_bluechip, earmark_token) =
        match CREATOR_EXCESS_POSITION.may_load(&deps.storage).unwrap() {
            Some(p) => (p.bluechip_amount, p.token_amount),
            None => (Uint128::zero(), Uint128::zero()),
        };

    Ok(CrossingOutcome {
        seed_osmo,
        seed_creator,
        // The mock reports no live poolmanager fee, so the charge is the
        // configured value.
        creation_fee: Uint128::new(configured_fee),
        leftover,
        earmark_bluechip,
        earmark_token,
        reserved_after: BLUECHIP_FEE_RESERVED.load(&deps.storage).unwrap(),
        threshold_hit_after: IS_THRESHOLD_HIT.load(&deps.storage).unwrap(),
    })
}

proptest! {
    // Ranges are chosen so the seeded OSMO is always positive (base seed
    // dwarfs the creation fee), exercising both the over-cap
    // (`raised_net > max_lock`) and non-over-cap branches. The seed-zero
    // error path (fee >= raise) is covered by the concrete test below.
    #![proptest_config(ProptestConfig::with_cases(400))]
    #[test]
    fn prop_threshold_crossing_conserves_osmo_and_earmarks_excess(
        raised_net in 3_000_000_000u128..=100_000_000_000u128,
        max_lock in 3_000_000_000u128..=50_000_000_000u128,
        configured_fee in 0u128..=2_000_000_000u128,
        // Reserved is bounded above by the target in the real funding flow.
        reserved_frac in 0.0f64..=1.0f64,
    ) {
        let reserved = (configured_fee as f64 * reserved_frac) as u128;
        let out = run_crossing(raised_net, max_lock, configured_fee, reserved)
            .expect("seed is positive across these ranges");

        let raised = Uint128::new(raised_net);
        let max_lock_u = Uint128::new(max_lock);
        let reserved_u = Uint128::new(reserved);

        // (1) OSMO conservation: the whole pre-crossing balance is accounted
        // for with nothing minted or lost.
        let out_side = out.seed_osmo
            + out.creation_fee
            + out.leftover
            + out.earmark_bluechip;
        prop_assert_eq!(
            out_side,
            raised + reserved_u,
            "OSMO not conserved: seed {} + fee {} + leftover {} + earmark {} != raised {} + reserved {}",
            out.seed_osmo, out.creation_fee, out.leftover, out.earmark_bluechip, raised, reserved_u
        );

        // (2) The bluechip earmark is the TRUE over-cap excess, never reduced
        // by the fee shortfall the protocol absorbs.
        let expected_earmark = raised.saturating_sub(max_lock_u);
        prop_assert_eq!(out.earmark_bluechip, expected_earmark);

        // (3) Over-cap iff raised > max_lock; creator-token side splits to match.
        if raised > max_lock_u {
            let expected_token =
                Uint128::new(POOL_SEED).multiply_ratio(raised - max_lock_u, raised);
            prop_assert_eq!(out.earmark_token, expected_token, "over-cap creator earmark");
            prop_assert_eq!(
                out.seed_creator,
                Uint128::new(POOL_SEED) - expected_token,
                "seeded creator side = pool_seed - earmarked creator tokens"
            );
        } else {
            prop_assert!(out.earmark_bluechip.is_zero(), "no bluechip earmark under cap");
            prop_assert!(out.earmark_token.is_zero(), "no creator earmark under cap");
            prop_assert_eq!(
                out.seed_creator,
                Uint128::new(POOL_SEED),
                "under cap the full pool-seed creator amount is seeded"
            );
        }

        // (4) Seed is always strictly positive on this input space.
        prop_assert!(!out.seed_osmo.is_zero(), "seed_osmo must be positive");

        // (5) Post-crossing bookkeeping: reserve pinned to the fee, threshold flipped.
        prop_assert_eq!(out.reserved_after, out.creation_fee);
        prop_assert!(out.threshold_hit_after);
    }
}

/// Edge case: when the creation fee meets or exceeds the raised seed, the
/// crossing surfaces the explicit, actionable `InvalidThresholdParams` error
/// (threshold mis-sized vs the chain fee) rather than an opaque gamm failure
/// or a zero-amount pool side. Mirrors the H-01 guard.
#[test]
fn crossing_rejects_when_fee_meets_or_exceeds_seed() {
    // raise = 1000, max_lock huge (non-over-cap so base_seed = raise), fee = 1000.
    let err = run_crossing(1_000, 10_000_000_000, 1_000, 0).unwrap_err();
    match err {
        ContractError::InvalidThresholdParams { msg } => {
            assert!(
                msg.contains("pool-creation fee"),
                "expected fee-vs-seed message, got: {}",
                msg
            );
        }
        other => panic!("expected InvalidThresholdParams, got {:?}", other),
    }
}

/// Concrete non-over-cap conservation check with a real shortfall
/// (`reserved < creation_fee`): the protocol absorbs the shortfall via a
/// smaller seed, and OSMO still balances exactly.
#[test]
fn crossing_shortfall_reduces_seed_not_earmark() {
    // raise 20_000, max_lock 50_000 (non-over-cap), fee 1_000, reserved 400.
    let out = run_crossing(20_000_000_000, 50_000_000_000, 1_000_000_000, 400_000_000).unwrap();
    // shortfall = 1_000 - 400 = 600; seed = 20_000 - 600 = 19_400.
    assert_eq!(out.seed_osmo, Uint128::new(19_400_000_000));
    assert_eq!(out.creation_fee, Uint128::new(1_000_000_000));
    assert!(out.leftover.is_zero(), "reserved < fee ⇒ nothing to remit");
    assert!(out.earmark_bluechip.is_zero(), "under cap ⇒ no earmark");
    // Conservation: 19_400 + 1_000 + 0 + 0 == 20_000 + 400.
    assert_eq!(
        out.seed_osmo + out.creation_fee,
        Uint128::new(20_000_000_000) + Uint128::new(400_000_000)
    );
}
