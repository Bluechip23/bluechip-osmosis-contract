// ---------------------------------------------------------------------------
// Cross-denom GAMM-creation-fee budgeting via the pricing pool's TWAP.
//
// The ONLY price read in the protocol, and it does not price the commit
// threshold (which is native-denominated). At threshold crossing, chains
// like osmosis-1 charge the x/gamm pool-creation fee in a NON-native denom
// (alloyed USDC). The pool contract retains its 1% commit fee in the native
// denom and must acquire the fee coin with an exact-amount-out swap through
// `pricing_pool_id`. That swap needs a native budget (its `token_in_max`),
// which this module values at the pricing pool's own arithmetic TWAP over
// `FEE_TWAP_WINDOW_SECONDS`.
//
// Why a TWAP is safe HERE when it was rejected for the threshold: the
// blast radius is the fee, not the raise. A manipulated TWAP either
// (a) under-budgets → the exact-out swap exceeds `token_in_max` → the whole
// crossing reverts atomically and is retried later (liveness, funds safe),
// or (b) over-budgets → the pool side hard-clamps the swap's `token_in_max`
// to its 1%-retention reserve (`BLUECHIP_FEE_RESERVED`), so the worst case
// spends protocol fee revenue only — never committer funds. The
// `MAX_NATIVE_PER_FEE_UNIT` ceiling below additionally bounds how far the
// reserve target itself can be inflated. Manipulating a 600s TWAP costs
// more than either outcome is worth.
//
// Fail-closed: any query error propagates and the commit reverts.
// ---------------------------------------------------------------------------

use cosmwasm_std::{Deps, Env, StdError, StdResult, Uint128};
use osmosis_std::types::osmosis::twap::v1beta1::TwapQuerier;

use crate::state::{FactoryInstantiate, FEE_TWAP_WINDOW_SECONDS};

/// Ceiling on the TWAP price (native base units PER fee base unit): reject
/// absurd outputs (a broken or manipulated pool) rather than letting a
/// nonsense budget through. 1_000 uosmo/uusdc corresponds to OSMO =
/// $0.001 — 40x below current spot (~$0.04), so legitimate price moves
/// have huge headroom, while the worst admissible budget for a 20-USDC fee
/// is capped at 20_000 OSMO. Defense-in-depth: the pool side additionally
/// clamps the swap's `token_in_max` to its 1%-retention reserve, so this
/// ceiling bounds reserve mis-sizing, not spendable funds.
const MAX_NATIVE_PER_FEE_UNIT: u128 = 1_000;

/// The chain's LIVE `x/poolmanager` pool-creation fee (first coin of the
/// params list — Osmosis has always configured exactly one), or `None`
/// when the query is unavailable (test mocks) or the list is empty.
/// Mirrors the pool side's `query_pool_creation_fee_coin`: the crossing
/// charges the LIVE fee, so the budget must be sized against it — sizing
/// against only the factory-configured coin would let config drift (or a
/// chain-governance fee change) strand a crossing without a budget.
pub fn query_live_pool_creation_fee(
    querier: &cosmwasm_std::QuerierWrapper,
) -> Option<cosmwasm_std::Coin> {
    use osmosis_std::types::osmosis::poolmanager::v1beta1::PoolmanagerQuerier;
    let params = PoolmanagerQuerier::new(querier).params().ok()?.params?;
    let first = params.pool_creation_fee.first()?;
    let amount = first.amount.parse::<u128>().ok()?;
    Some(cosmwasm_std::Coin {
        denom: first.denom.clone(),
        amount: Uint128::new(amount),
    })
}

/// Native base units needed to buy `fee_amount` of `fee_quote_denom` through
/// `pricing_pool_id`, valued at the pool's arithmetic TWAP over the trailing
/// `FEE_TWAP_WINDOW_SECONDS`, WITHOUT margin (the pool contract applies its
/// own swap margin on top).
///
/// The TWAP is quoted as native-per-fee-unit by asking the chain for the
/// price of the FEE denom (base) in the NATIVE denom (quote): both are
/// 6-decimal denoms on osmosis-1, so `budget = fee_amount × price`.
pub fn fee_swap_budget_native(
    deps: Deps,
    env: &Env,
    config: &FactoryInstantiate,
    fee_amount: Uint128,
) -> StdResult<Uint128> {
    if config.pricing_pool_id == 0 {
        return Err(StdError::generic_err(
            "cross-denom gamm fee configured but pricing_pool_id is 0",
        ));
    }
    if config.fee_quote_denom.is_empty() {
        return Err(StdError::generic_err(
            "cross-denom gamm fee configured but fee_quote_denom is empty",
        ));
    }
    let start = env
        .block
        .time
        .seconds()
        .saturating_sub(FEE_TWAP_WINDOW_SECONDS);
    let resp = TwapQuerier::new(&deps.querier)
        .arithmetic_twap_to_now(
            config.pricing_pool_id,
            // base = fee denom, quote = native → price is native per fee unit.
            config.fee_quote_denom.clone(),
            config.bluechip_denom.clone(),
            Some(osmosis_std::shim::Timestamp {
                seconds: start as i64,
                nanos: 0,
            }),
        )
        .map_err(|e| {
            StdError::generic_err(format!(
                "fee-swap TWAP query failed for pool {}: {}",
                config.pricing_pool_id, e
            ))
        })?;
    twap_price_to_budget(&resp.arithmetic_twap, fee_amount)
}

/// Multiply a decimal-string TWAP price (native per fee unit) by the fee
/// amount, rounding UP so the budget never undershoots by a base unit.
/// Rejects non-positive and implausibly large prices (fail closed).
pub fn twap_price_to_budget(twap: &str, fee_amount: Uint128) -> StdResult<Uint128> {
    let price: cosmwasm_std::Decimal = twap
        .trim()
        .parse()
        .map_err(|_| StdError::generic_err(format!("unparseable TWAP price: {twap:?}")))?;
    if price.is_zero() {
        return Err(StdError::generic_err(
            "fee-swap TWAP price is zero — pricing pool unusable",
        ));
    }
    if price > cosmwasm_std::Decimal::from_ratio(MAX_NATIVE_PER_FEE_UNIT, 1u128) {
        return Err(StdError::generic_err(format!(
            "fee-swap TWAP price {price} exceeds plausibility ceiling {MAX_NATIVE_PER_FEE_UNIT}"
        )));
    }
    // ceil(fee_amount × price) in 256-bit space so no plausible input
    // can overflow the intermediate product.
    let num = price.atomics().full_mul(fee_amount);
    let denom = cosmwasm_std::Uint256::from(10u128.pow(18));
    let budget = num
        .checked_add(denom - cosmwasm_std::Uint256::one())
        .map_err(|_| StdError::generic_err("fee budget overflow"))?
        / denom;
    Uint128::try_from(budget).map_err(|_| StdError::generic_err("fee budget overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE_20_USDC: Uint128 = Uint128::new(20_000_000);

    #[test]
    fn budget_multiplies_and_ceils() {
        // 25 uosmo per uusdc × 20 USDC = exactly 500 OSMO.
        assert_eq!(
            twap_price_to_budget("25.0", FEE_20_USDC).unwrap(),
            Uint128::new(500_000_000)
        );
        // Fractional remainder must round UP, never down: the budget may
        // not undershoot the swap's need by even one base unit.
        assert_eq!(
            twap_price_to_budget("0.0000001", Uint128::new(3)).unwrap(),
            Uint128::one(),
            "3 × 1e-7 = 3e-7 must ceil to 1"
        );
        // Exact products must NOT get an extra unit from the ceil.
        assert_eq!(
            twap_price_to_budget("0.5", Uint128::new(4)).unwrap(),
            Uint128::new(2)
        );
    }

    #[test]
    fn budget_rejects_zero_and_garbage() {
        assert!(twap_price_to_budget("0", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("0.0", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("not-a-number", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("-1", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("1e5", FEE_20_USDC).is_err());
    }

    #[test]
    fn budget_enforces_plausibility_ceiling() {
        // Exactly AT the ceiling passes (1_000 native per fee unit).
        assert_eq!(
            twap_price_to_budget("1000", FEE_20_USDC).unwrap(),
            Uint128::new(20_000_000_000),
            "worst admissible budget for a 20-USDC fee is 20,000 OSMO"
        );
        // One atto above the ceiling fails — a manipulated/broken pool
        // cannot push an implausible reserve target through.
        assert!(twap_price_to_budget("1000.000000000000000001", FEE_20_USDC).is_err());
        assert!(twap_price_to_budget("100000", FEE_20_USDC).is_err());
    }

    #[test]
    fn budget_overflow_fails_closed() {
        // Max admissible price × max fee amount overflows Uint128 — must
        // error, never truncate.
        assert!(twap_price_to_budget("1000", Uint128::MAX).is_err());
    }
}
