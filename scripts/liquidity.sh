#!/usr/bin/env bash
# =====================================================================
# liquidity.sh — LP lifecycle on the NATIVE GAMM pool of a
#                threshold-crossed commit pool
# =====================================================================
# usage:
#   scripts/liquidity.sh deposit <pool_addr> <native-micro>
#   scripts/liquidity.sh shares  <pool_addr> [owner]
#   scripts/liquidity.sh remove  <pool_addr> [share-amount]
#
# Post-migration, third-party liquidity lives on the native Osmosis
# GAMM pool the contract seeded at threshold crossing — NOT in the
# contract. The contract is only used to DISCOVER the pool
# ({"native_pool_id":{}} -> pool_id + the gamm/pool/{id} share denom);
# add/remove are chain-native MsgJoinPool / MsgExitPool. The old
# deposit_liquidity / position-NFT / CW20-allowance flow is gone.
#
# deposit:  two-sided ratio-matched join. <native-micro> sizes the
#           OSMO leg; the matching creator-token amount is computed
#           from live pool reserves and both max-in caps get ~1%
#           headroom for ratio drift. You must hold BOTH sides (buy
#           creator tokens first: scripts/swap.sh <pool> native <amt>).
#           Prints the gamm shares received.
# shares:   prints [owner]'s gamm/pool/{id} share balance
#           (default owner: your key).
# remove:   MsgExitPool burning [share-amount] shares (default: your
#           entire balance); both assets return to your wallet.
#
# Native-module txs — NOT subject to the contract's 13s per-wallet
# rate limit (that only gates contract calls like commit/swap).
#
# Only works POST-threshold — pre-threshold pools have no native pool.
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
    sed -n '6,9p' "$0" | sed 's/^# //' >&2
    exit 1
fi

# Resolve the native GAMM pool id + LP share denom from the contract.
NATIVE_INFO="$(query_smart "$POOL_ADDR" '{"native_pool_id":{}}')"
POOL_ID="$(echo "$NATIVE_INFO" | jq -r '.pool_id // empty')"
LP_DENOM="$(echo "$NATIVE_INFO" | jq -r '.lp_share_denom // empty')"
if [ -z "$POOL_ID" ]; then
    echo "error: pool has no native GAMM pool yet (pre-threshold?): $NATIVE_INFO" >&2
    exit 1
fi
LP_DENOM="${LP_DENOM:-gamm/pool/$POOL_ID}"

# Live reserves + total shares from the gamm module.
pool_json() { query_json gamm pool "$POOL_ID"; }
share_balance() {
    query_json bank balances "$1" | jq -r --arg d "$LP_DENOM" \
        '.balances[]? | select(.denom == $d) | .amount' | head -1
}

case "$CMD" in
deposit)
    AMOUNT0="${3:-}"   # native side; token side is ratio-derived
    if [ -z "$AMOUNT0" ]; then
        echo "usage: $0 deposit <pool_addr> <native-micro>" >&2
        exit 1
    fi
    POOL_JSON="$(pool_json)"
    TOTAL_SHARES="$(echo "$POOL_JSON" | jq -r '.pool.total_shares.amount')"
    R0="$(echo "$POOL_JSON" | jq -r --arg d "$NATIVE_DENOM" \
        '.pool.pool_assets[] | select(.token.denom == $d) | .token.amount')"
    TOKEN_DENOM="$(echo "$POOL_JSON" | jq -r --arg d "$NATIVE_DENOM" \
        '.pool.pool_assets[] | select(.token.denom != $d) | .token.denom')"
    R1="$(echo "$POOL_JSON" | jq -r --arg d "$NATIVE_DENOM" \
        '.pool.pool_assets[] | select(.token.denom != $d) | .token.amount')"
    if [ -z "$TOTAL_SHARES" ] || [ -z "$R0" ] || [ "$R0" = "0" ]; then
        echo "error: cannot read pool $POOL_ID reserves: $POOL_JSON" >&2
        exit 1
    fi

    # share_out = total * amount0/reserve0, shaved 1% so the ratio-matched
    # max-in caps (amount + 1%) always cover what the module pulls.
    SHARE_OUT="$(awk -v t="$TOTAL_SHARES" -v a="$AMOUNT0" -v r="$R0" \
        'BEGIN { printf "%.0f", t / r * a * 0.99 }')"
    AMOUNT1_MAX="$(awk -v a="$AMOUNT0" -v r0="$R0" -v r1="$R1" \
        'BEGIN { printf "%.0f", a * r1 / r0 * 1.01 + 1 }')"
    if [ "$SHARE_OUT" = "0" ]; then
        echo "error: deposit too small — computes to zero shares" >&2
        exit 1
    fi

    echo "native pool:   $POOL_ID ($LP_DENOM)"
    echo "reserves:      $R0 $NATIVE_DENOM / $R1 $TOKEN_DENOM"
    echo "joining:       <= $AMOUNT0 $NATIVE_DENOM + <= $AMOUNT1_MAX token for $SHARE_OUT shares"

    BEFORE="$(share_balance "$ADDR")"; BEFORE="${BEFORE:-0}"
    RESULT="$(submit_tx gamm join-pool \
        --pool-id "$POOL_ID" \
        --share-amount-out "$SHARE_OUT" \
        --max-amounts-in "${AMOUNT0}${NATIVE_DENOM}" \
        --max-amounts-in "${AMOUNT1_MAX}${TOKEN_DENOM}")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    AFTER="$(share_balance "$ADDR")"; AFTER="${AFTER:-0}"
    echo "gamm shares:   $BEFORE -> $AFTER (+$((AFTER - BEFORE)))"
    ;;

shares)
    OWNER="${3:-$ADDR}"
    BAL="$(share_balance "$OWNER")"
    echo "${BAL:-0}"
    ;;

remove)
    SHARE_IN="${3:-}"
    if [ -z "$SHARE_IN" ]; then
        SHARE_IN="$(share_balance "$ADDR")"
        if [ -z "$SHARE_IN" ] || [ "$SHARE_IN" = "0" ]; then
            echo "error: no $LP_DENOM shares to remove" >&2
            exit 1
        fi
        echo "removing entire balance: $SHARE_IN shares"
    fi
    RESULT="$(submit_tx gamm exit-pool \
        --pool-id "$POOL_ID" \
        --share-amount-in "$SHARE_IN" \
        --min-amounts-out "1${NATIVE_DENOM}")"
    echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
    REMAINING="$(share_balance "$ADDR")"
    echo "remaining shares: ${REMAINING:-0}"
    ;;

*)
    echo "error: unknown subcommand '$CMD' (deposit|shares|remove)" >&2
    exit 1
    ;;
esac
