//! F-5 — reentrancy-guard unit tests for `ExecuteMultiHop`.
//!
//! The full success path (guard set at route start, cleared by the terminal
//! `AssertReceived`) is already exercised end-to-end by every route in
//! `integration_tests` — those pass, which proves the guard clears correctly
//! on a normal route. This file pins the REJECTION: a nested
//! `ExecuteMultiHop` while a route is already in progress (what a malicious
//! pool called mid-hop would attempt) is refused.

use cosmwasm_std::testing::{message_info, mock_dependencies, mock_env};
use cosmwasm_std::{Addr, Coin, Uint128};
use pool_factory_interfaces::asset::TokenType;
use pool_factory_interfaces::routing::SwapOperation;

use crate::error::RouterError;
use crate::execution::execute_multi_hop;
use crate::state::ROUTE_IN_PROGRESS;

fn one_hop() -> Vec<SwapOperation> {
    vec![SwapOperation {
        pool_addr: "pool1".to_string(),
        offer_asset_info: TokenType::Native {
            denom: "ubluechip".to_string(),
        },
        ask_asset_info: TokenType::Native {
            denom: "factory/creator/ucreator".to_string(),
        },
    }]
}

/// A nested `ExecuteMultiHop` (guard already set — i.e. a pool re-entering the
/// router mid-route) is rejected with `Reentrancy`, before any route setup.
#[test]
fn nested_multi_hop_is_rejected_while_route_in_progress() {
    let mut deps = mock_dependencies();
    // Simulate an outer route already in flight.
    ROUTE_IN_PROGRESS.save(&mut deps.storage, &true).unwrap();

    let info = message_info(
        &Addr::unchecked("attacker_pool"),
        &[Coin {
            denom: "ubluechip".to_string(),
            amount: Uint128::new(1_000),
        }],
    );

    let err = execute_multi_hop(
        deps.as_mut(),
        mock_env(),
        info,
        one_hop(),
        Uint128::new(1), // non-zero minimum_receive
        None,
        None,
    )
    .unwrap_err();

    assert!(
        matches!(err, RouterError::Reentrancy),
        "a nested ExecuteMultiHop must be rejected with Reentrancy; got {err:?}"
    );
    // The guard is untouched by the rejected reentrant call (still set by the
    // outer route).
    assert!(
        ROUTE_IN_PROGRESS.load(&deps.storage).unwrap(),
        "the in-progress flag must remain set after rejecting the nested route"
    );
}
