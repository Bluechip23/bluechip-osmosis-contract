#!/usr/bin/env bash
# =====================================================================
# swap.sh — trade against a threshold-crossed commit pool's AMM
# =====================================================================
# usage: scripts/swap.sh <pool_addr> native <amount-micro>   # OSMO -> creator token
#        scripts/swap.sh <pool_addr> token  <amount-micro>   # creator token -> OSMO
#
# Simulates first (pool.Simulation) and prints the expected return,
# then executes pool.SimpleSwap with the offer coin attached as funds.
# Both directions are plain native transfers now: the creator token is
# a TokenFactory bank denom (factory/{pool}/{sub}), so selling it is
# the same SimpleSwap shape with that denom attached — the old CW20
# Send/Receive hook is gone with the migration. The contract routes
# the swap through the native GAMM pool via MsgSwapExactAmountIn.
#
# max_spread is pinned to the pools' 5% hard cap so thin testnet pools
# don't trip the 0.5% default. Swaps are rate-limited to one per wallet
# per 13s (DEFAULT_SWAP_RATE_LIMIT_SECS) — space out repeat runs.
#
# Only works POST-threshold: pre-threshold pools reject swaps.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

POOL_ADDR="${1:-}"
SIDE="${2:-}"
AMOUNT="${3:-}"
if [ -z "$POOL_ADDR" ] || [ -z "$AMOUNT" ] \
    || { [ "$SIDE" != "native" ] && [ "$SIDE" != "token" ]; }; then
    echo "usage: $0 <pool_addr> native|token <amount-micro>" >&2
    exit 1
fi

# Resolve the creator-token TokenFactory denom from the pool's pair info.
PAIR="$(query_smart "$POOL_ADDR" '{"pair":{}}')"
TOKEN_DENOM="$(echo "$PAIR" | jq -r '
    [ .. | objects | .creator_token? | select(. != null) | .denom ]
    | first // empty')"
if [ -z "$TOKEN_DENOM" ]; then
    echo "error: could not resolve creator token from pair query: $PAIR" >&2
    exit 1
fi

if [ "$SIDE" = "native" ]; then
    OFFER_INFO="$(jq -nc --arg d "$NATIVE_DENOM" '{bluechip:{denom:$d}}')"
    OFFER_DENOM="$NATIVE_DENOM"
else
    OFFER_INFO="$(jq -nc --arg d "$TOKEN_DENOM" '{creator_token:{denom:$d}}')"
    OFFER_DENOM="$TOKEN_DENOM"
fi

# ---- Simulate --------------------------------------------------------
SIM_MSG="$(jq -nc --argjson info "$OFFER_INFO" --arg amt "$AMOUNT" \
    '{simulation:{offer_asset:{info:$info, amount:$amt}}}')"
SIM="$(query_smart "$POOL_ADDR" "$SIM_MSG")"
EXPECTED="$(echo "$SIM" | jq -r '.return_amount // empty' 2>/dev/null || true)"
echo "pool:          $POOL_ADDR"
echo "creator token: $TOKEN_DENOM"
echo "offering:      $AMOUNT ($SIDE side)"
echo "simulation:    $SIM"
echo ""

# ---- Execute ----------------------------------------------------------
# Same SimpleSwap shape both directions; only the attached coin differs.
SWAP_MSG="$(jq -nc --argjson info "$OFFER_INFO" --arg amt "$AMOUNT" \
    '{simple_swap:{
        offer_asset:{info:$info, amount:$amt},
        belief_price:null,
        max_spread:"0.05",
        to:null,
        transaction_deadline:null
    }}')"
RESULT="$(submit_tx wasm execute "$POOL_ADDR" "$SWAP_MSG" \
    --amount "${AMOUNT}${OFFER_DENOM}")"

echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"
RETURNED="$(extract_attr "$RESULT" wasm return_amount)"
[ -n "$EXPECTED" ] && echo "expected return: $EXPECTED"
[ -n "$RETURNED" ] && echo "actual return:   $RETURNED"
