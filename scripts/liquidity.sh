#!/usr/bin/env bash
# =====================================================================
# liquidity.sh — LP lifecycle against a threshold-crossed commit pool
# =====================================================================
# usage:
#   scripts/liquidity.sh deposit   <pool_addr> <native-micro> <token-micro>
#   scripts/liquidity.sh positions <pool_addr> [owner]
#   scripts/liquidity.sh collect   <pool_addr> <position_id>
#   scripts/liquidity.sh remove    <pool_addr> <position_id>
#
# deposit:   amount0 is the NATIVE_DENOM side (attached as funds),
#            amount1 the creator-token side. The pool pulls the CW20 leg
#            via TransferFrom, so this script sends increase_allowance
#            to the token first. Over-paid native is refunded; the CW20
#            side pulls exactly what the ratio needs. A position NFT is
#            minted to the caller; the position_id is printed.
# positions: lists positions owned by [owner] (default: your key).
# collect:   claims accrued LP fees on a position.
# remove:    removes ALL liquidity from a position and burns it.
#
# Only works POST-threshold — pre-threshold pools have no AMM to LP into.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

CMD="${1:-}"
POOL_ADDR="${2:-}"
if [ -z "$CMD" ] || [ -z "$POOL_ADDR" ]; then
    sed -n '5,10p' "$0" | sed 's/^# //' >&2
    exit 1
fi

resolve_token() {
    query_smart "$POOL_ADDR" '{"pair":{}}' | jq -r '
        [ .. | objects | .creator_token? | select(. != null) | .contract_addr ]
        | first // empty'
}

case "$CMD" in
deposit)
    AMOUNT0="${3:-}"   # native side
    AMOUNT1="${4:-}"   # creator-token side
    if [ -z "$AMOUNT0" ] || [ -z "$AMOUNT1" ]; then
        echo "usage: $0 deposit <pool_addr> <native-micro> <token-micro>" >&2
        exit 1
    fi
    TOKEN_ADDR="$(resolve_token)"
    [ -z "$TOKEN_ADDR" ] && { echo "error: cannot resolve creator token" >&2; exit 1; }

    echo "approving pool to pull $AMOUNT1 from $TOKEN_ADDR"
    ALLOW_MSG="$(jq -nc --arg s "$POOL_ADDR" --arg a "$AMOUNT1" \
        '{increase_allowance:{spender:$s, amount:$a}}')"
    submit_tx wasm execute "$TOKEN_ADDR" "$ALLOW_MSG" >/dev/null
    echo "depositing amount0=$AMOUNT0 $NATIVE_DENOM, amount1=$AMOUNT1 token"
    DEPOSIT_MSG="$(jq -nc --arg a0 "$AMOUNT0" --arg a1 "$AMOUNT1" \
        '{deposit_liquidity:{
            amount0:$a0, amount1:$a1,
            min_amount0:null, min_amount1:null,
            transaction_deadline:null
        }}')"
    RESULT="$(submit_tx wasm execute "$POOL_ADDR" "$DEPOSIT_MSG" \
        --amount "${AMOUNT0}${NATIVE_DENOM}")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    POSITION_ID="$(extract_attr "$RESULT" wasm position_id)"
    [ -n "$POSITION_ID" ] && echo "position_id: $POSITION_ID"
    ;;

positions)
    OWNER="${3:-$ADDR}"
    Q="$(jq -nc --arg o "$OWNER" \
        '{positions_by_owner:{owner:$o, start_after:null, limit:null}}')"
    query_smart "$POOL_ADDR" "$Q" | jq .
    ;;

collect)
    POSITION_ID="${3:-}"
    [ -z "$POSITION_ID" ] && { echo "usage: $0 collect <pool_addr> <position_id>" >&2; exit 1; }
    MSG="$(jq -nc --arg p "$POSITION_ID" \
        '{collect_fees:{position_id:$p, transaction_deadline:null}}')"
    RESULT="$(submit_tx wasm execute "$POOL_ADDR" "$MSG")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    ;;

remove)
    POSITION_ID="${3:-}"
    [ -z "$POSITION_ID" ] && { echo "usage: $0 remove <pool_addr> <position_id>" >&2; exit 1; }
    MSG="$(jq -nc --arg p "$POSITION_ID" \
        '{remove_all_liquidity:{
            position_id:$p,
            transaction_deadline:null,
            min_amount0:null, min_amount1:null,
            max_ratio_deviation_bps:null
        }}')"
    RESULT="$(submit_tx wasm execute "$POOL_ADDR" "$MSG")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    ;;

*)
    echo "error: unknown subcommand '$CMD' (deposit|positions|collect|remove)" >&2
    exit 1
    ;;
esac
