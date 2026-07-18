#!/usr/bin/env bash
# =====================================================================
# route_swap.sh — multi-hop swaps through the router
# =====================================================================
# usage:
#   scripts/route_swap.sh buy <pool_addr> <native-micro>
#       Single hop OSMO -> creator token, routed through the router's
#       ExecuteMultiHop (native-offered entry path).
#
#   scripts/route_swap.sh hop <pool_a> <pool_b> <tokenA-micro>
#       Two hops tokenA -> OSMO -> tokenB. Creator tokens are native
#       TokenFactory denoms post-migration, so this is the same
#       ExecuteMultiHop entry with tokenA attached as funds (the old
#       CW20 Receive path was removed from the router). Both pools
#       must be threshold-crossed.
#
# Each route is simulated first (router.SimulateMultiHop); the swap's
# minimum_receive is set to 99% of the simulated final amount. The
# router validates every pool against the factory registry and pins
# per-hop max_spread to the pools' 5% hard cap — minimum_receive is
# the binding slippage control.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

if [ -z "${ROUTER_ADDR:-}" ]; then
    echo "error: ROUTER_ADDR not in $STATE_FILE — re-run ./deploy_osmosis.sh" >&2
    exit 1
fi

CMD="${1:-}"

pool_token() {
    query_smart "$1" '{"pair":{}}' | jq -r '
        [ .. | objects | .creator_token? | select(. != null) | .denom ]
        | first // empty'
}

simulate() {
    local ops="$1" amount="$2"
    local q sim
    q="$(jq -nc --argjson ops "$ops" --arg amt "$amount" \
        '{simulate_multi_hop:{operations:$ops, offer_amount:$amt}}')"
    sim="$(query_smart "$ROUTER_ADDR" "$q")"
    echo "simulation: $sim" >&2
    echo "$sim" | jq -r '.final_amount // empty'
}

min_receive() {
    awk -v f="$1" 'BEGIN { printf "%.0f", f * 0.99 }'
}

case "$CMD" in
buy)
    POOL_ADDR="${2:-}"
    AMOUNT="${3:-}"
    if [ -z "$POOL_ADDR" ] || [ -z "$AMOUNT" ]; then
        echo "usage: $0 buy <pool_addr> <native-micro>" >&2
        exit 1
    fi
    TOKEN="$(pool_token "$POOL_ADDR")"
    [ -z "$TOKEN" ] && { echo "error: cannot resolve creator token for $POOL_ADDR" >&2; exit 1; }

    OPS="$(jq -nc --arg pool "$POOL_ADDR" --arg d "$NATIVE_DENOM" --arg t "$TOKEN" \
        '[{
            pool_addr:$pool,
            offer_asset_info:{bluechip:{denom:$d}},
            ask_asset_info:{creator_token:{denom:$t}}
        }]')"
    FINAL="$(simulate "$OPS" "$AMOUNT")"
    [ -z "$FINAL" ] && { echo "error: simulation returned no final_amount" >&2; exit 1; }
    MIN="$(min_receive "$FINAL")"

    EXEC="$(jq -nc --argjson ops "$OPS" --arg min "$MIN" \
        '{execute_multi_hop:{operations:$ops, minimum_receive:$min, deadline:null, recipient:null}}')"
    RESULT="$(submit_tx wasm execute "$ROUTER_ADDR" "$EXEC" \
        --amount "${AMOUNT}${NATIVE_DENOM}")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    echo "expected >= $MIN (simulated $FINAL) of $TOKEN"
    ;;

hop)
    POOL_A="${2:-}"
    POOL_B="${3:-}"
    AMOUNT="${4:-}"
    if [ -z "$POOL_A" ] || [ -z "$POOL_B" ] || [ -z "$AMOUNT" ]; then
        echo "usage: $0 hop <pool_a> <pool_b> <tokenA-micro>" >&2
        exit 1
    fi
    TOKEN_A="$(pool_token "$POOL_A")"
    TOKEN_B="$(pool_token "$POOL_B")"
    if [ -z "$TOKEN_A" ] || [ -z "$TOKEN_B" ]; then
        echo "error: cannot resolve creator tokens (a=$TOKEN_A b=$TOKEN_B)" >&2
        exit 1
    fi

    OPS="$(jq -nc \
        --arg pa "$POOL_A" --arg pb "$POOL_B" \
        --arg ta "$TOKEN_A" --arg tb "$TOKEN_B" \
        --arg d "$NATIVE_DENOM" \
        '[
            {
                pool_addr:$pa,
                offer_asset_info:{creator_token:{denom:$ta}},
                ask_asset_info:{bluechip:{denom:$d}}
            },
            {
                pool_addr:$pb,
                offer_asset_info:{bluechip:{denom:$d}},
                ask_asset_info:{creator_token:{denom:$tb}}
            }
        ]')"
    FINAL="$(simulate "$OPS" "$AMOUNT")"
    [ -z "$FINAL" ] && { echo "error: simulation returned no final_amount" >&2; exit 1; }
    MIN="$(min_receive "$FINAL")"

    EXEC="$(jq -nc --argjson ops "$OPS" --arg min "$MIN" \
        '{execute_multi_hop:{operations:$ops, minimum_receive:$min, deadline:null, recipient:null}}')"
    RESULT="$(submit_tx wasm execute "$ROUTER_ADDR" "$EXEC" \
        --amount "${AMOUNT}${TOKEN_A}")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    echo "expected >= $MIN (simulated $FINAL) of $TOKEN_B"
    ;;

*)
    echo "usage: $0 buy <pool_addr> <native-micro>" >&2
    echo "       $0 hop <pool_a> <pool_b> <tokenA-micro>" >&2
    exit 1
    ;;
esac
