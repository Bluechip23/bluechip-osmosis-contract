#!/usr/bin/env bash
# =====================================================================
# run_lifecycle_test.sh — full protocol lifecycle against a deployed
#                         testnet stack, end to end
# =====================================================================
# usage: scripts/run_lifecycle_test.sh
#
# Exercises the whole surface the docs call out for a testnet
# rehearsal, in order:
#
#   1. create a commit pool (factory.Create)
#   2. make a small pre-threshold commit and verify USD accounting
#   3. commit past the USD threshold (auto-sized at the x/twap rate)
#   4. verify the crossing seeded the AMM (reserves > 0)
#   5. drain the committer distribution and verify the CW20 payout landed
#   6. swap native -> token and token -> native against the AMM
#   7. deposit liquidity (native + CW20 allowance), list positions,
#      remove all liquidity
#   8. route a swap through the router (OSMO -> token)
#
# Notes:
#   - Costs real testnet OSMO: crossing the threshold takes
#     COMMIT_THRESHOLD_LIMIT_USD worth at the live rate. Drop the
#     threshold to a few hundred dollars in osmo_testnet.env before
#     deploying, per docs/OSMOSIS_DEPLOY.md.
#   - Respects the per-wallet 13s commit/swap rate limit with sleeps.
#   - Pool creation is rate-limited to 1/hour/address on the prod
#     factory build; re-runs within the hour fail at step 1 unless you
#     deployed with FACTORY_WASM_FILE=factory-integration.wasm.
#   - Steps continue past failures; the summary at the end lists them.
# =====================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS="$REPO_ROOT/scripts"
# shellcheck disable=SC1091
source "$SCRIPTS/_helpers.sh"
ensure_tools
ensure_key
require_state

PASS=0
FAIL=0
FAILED_STEPS=()

step() {
    local name="$1"
    shift
    echo ""
    echo "───────────────────────────────────────────────────"
    echo "STEP: $name"
    echo "───────────────────────────────────────────────────"
    if "$@"; then
        echo "PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "FAIL: $name" >&2
        FAIL=$((FAIL + 1))
        FAILED_STEPS+=("$name")
    fi
}

rate_limit_pause() {
    # DEFAULT_SWAP_RATE_LIMIT_SECS = 13; commits and swaps share it.
    echo "(waiting out the 13s per-wallet rate limit)"
    sleep 15
}

# Suffix from the block height keeps re-runs unique without $RANDOM
# (which repeats across fast re-invocations).
SUFFIX="$(query_json block 2>/dev/null | jq -r '.header.height // empty' 2>/dev/null | tail -c 5)"
SUFFIX="${SUFFIX:-$$}"
SYMBOL="LFC${SUFFIX//[^0-9]/}"
NAME="Lifecycle Test $SYMBOL"

POOL_ADDR=""
TOKEN_ADDR=""

# ---- 1. create pool --------------------------------------------------
create_pool() {
    "$SCRIPTS/create_commit_pool.sh" "$NAME" "$SYMBOL" || return 1
    local line
    line="$(awk -F '\t' -v s="$SYMBOL" '$5 == s {found=$0} END {print found}' \
        "$REPO_ROOT/commit_pools.txt")"
    POOL_ADDR="$(echo "$line" | cut -f2)"
    TOKEN_ADDR="$(echo "$line" | cut -f3)"
    [ -n "$POOL_ADDR" ] && [ -n "$TOKEN_ADDR" ]
}
step "create commit pool" create_pool
if [ -z "$POOL_ADDR" ]; then
    echo ""
    echo "cannot continue without a pool — aborting" >&2
    exit 1
fi

# ---- 2. small pre-threshold commit ----------------------------------
small_commit() {
    # ~$6 at the live rate (min pre-threshold commit is $5).
    local probe usd_per_osmo amount raised_before raised_after
    probe="$(query_smart "$FACTORY_ADDR" \
        '{"pool_factory_query":{"convert_native_to_usd":{"amount":"1000000"}}}')"
    usd_per_osmo="$(echo "$probe" | jq -r '.amount // empty')"
    [ -z "$usd_per_osmo" ] && { echo "pricing probe failed: $probe" >&2; return 1; }
    amount="$(awk -v r="$usd_per_osmo" 'BEGIN { printf "%.0f", 6000000/r*1000000 + 1 }')"
    raised_before="$(query_smart "$POOL_ADDR" '{"is_fully_commited":{}}' \
        | jq -r '.in_progress.raised // "0"')"
    "$SCRIPTS/cross_threshold.sh" "$POOL_ADDR" "$amount" || return 1
    raised_after="$(query_smart "$POOL_ADDR" '{"is_fully_commited":{}}' \
        | jq -r '.in_progress.raised // empty')"
    # A tiny commit must not cross; raised must strictly increase.
    [ -n "$raised_after" ] && [ "$raised_after" -gt "$raised_before" ]
}
step "small pre-threshold commit (USD accounting)" small_commit
rate_limit_pause

# ---- 3. cross the threshold ------------------------------------------
step "cross the USD threshold" "$SCRIPTS/cross_threshold.sh" "$POOL_ADDR"

# ---- 4. AMM seeded ----------------------------------------------------
amm_seeded() {
    local status state r0 r1
    status="$(query_smart "$POOL_ADDR" '{"is_fully_commited":{}}')"
    echo "commit status: $status"
    echo "$status" | jq -e '
        (type == "string" and . == "fully_committed")
        or (type == "object" and has("fully_committed"))' >/dev/null 2>&1 \
        || { echo "pool is not fully committed" >&2; return 1; }
    state="$(query_smart "$POOL_ADDR" '{"pool_state":{}}')"
    r0="$(echo "$state" | jq -r '.reserve0 // "0"')"
    r1="$(echo "$state" | jq -r '.reserve1 // "0"')"
    echo "reserves: reserve0=$r0 reserve1=$r1"
    [ "$r0" != "0" ] && [ "$r1" != "0" ]
}
step "threshold crossing seeded the AMM" amm_seeded

# ---- 5. distribution --------------------------------------------------
distribution() {
    "$SCRIPTS/continue_distribution.sh" "$POOL_ADDR" || return 1
    local bal
    bal="$(query_smart "$TOKEN_ADDR" \
        "$(jq -nc --arg a "$ADDR" '{balance:{address:$a}}')" \
        | jq -r '.balance // "0"')"
    echo "committer creator-token balance: $bal"
    [ "$bal" != "0" ]
}
step "drain distribution + CW20 payout landed" distribution
rate_limit_pause

# ---- 6. swaps ----------------------------------------------------------
step "swap native -> token" "$SCRIPTS/swap.sh" "$POOL_ADDR" native 1000000
rate_limit_pause
step "swap token -> native" "$SCRIPTS/swap.sh" "$POOL_ADDR" token 1000000
rate_limit_pause

# ---- 7. liquidity -------------------------------------------------------
POSITION_ID=""
lp_deposit() {
    # Match the pool's current ratio: amount1 = amount0 * reserve1/reserve0.
    local state r0 r1 amount0 amount1 out
    state="$(query_smart "$POOL_ADDR" '{"pool_state":{}}')"
    r0="$(echo "$state" | jq -r '.reserve0 // "0"')"
    r1="$(echo "$state" | jq -r '.reserve1 // "0"')"
    [ "$r0" = "0" ] && return 1
    amount0=1000000
    amount1="$(awk -v a="$amount0" -v r0="$r0" -v r1="$r1" \
        'BEGIN { printf "%.0f", a * r1 / r0 + 1 }')"
    out="$("$SCRIPTS/liquidity.sh" deposit "$POOL_ADDR" "$amount0" "$amount1")" || return 1
    echo "$out"
    POSITION_ID="$(echo "$out" | awk -F': ' '/^position_id:/ {print $2}')"
    if [ -z "$POSITION_ID" ]; then
        POSITION_ID="$("$SCRIPTS/liquidity.sh" positions "$POOL_ADDR" \
            | jq -r '.positions[-1].position_id // empty')"
    fi
    [ -n "$POSITION_ID" ]
}
step "deposit liquidity (mints position NFT)" lp_deposit

lp_positions() {
    "$SCRIPTS/liquidity.sh" positions "$POOL_ADDR" | jq -e '.positions | length > 0' >/dev/null
}
step "positions listed for owner" lp_positions

if [ -n "$POSITION_ID" ]; then
    step "remove all liquidity" "$SCRIPTS/liquidity.sh" remove "$POOL_ADDR" "$POSITION_ID"
else
    echo "SKIP: remove liquidity (no position id captured)"
fi
rate_limit_pause

# ---- 8. router -----------------------------------------------------------
step "router single-hop (OSMO -> token)" "$SCRIPTS/route_swap.sh" buy "$POOL_ADDR" 1000000

# A 2-hop route needs a second threshold-crossed pool; run it when
# commit_pools.txt already has one from a previous lifecycle run.
SECOND_POOL="$(awk -F '\t' -v me="$POOL_ADDR" '$2 != me {print $2}' \
    "$REPO_ROOT/commit_pools.txt" 2>/dev/null | tail -n 1)"
if [ -n "$SECOND_POOL" ]; then
    second_crossed() {
        query_smart "$SECOND_POOL" '{"is_fully_commited":{}}' | jq -e '
            (type == "string" and . == "fully_committed")
            or (type == "object" and has("fully_committed"))' >/dev/null 2>&1
    }
    if second_crossed; then
        rate_limit_pause
        step "router two-hop (tokenA -> OSMO -> tokenB)" \
            "$SCRIPTS/route_swap.sh" hop "$POOL_ADDR" "$SECOND_POOL" 1000000
    else
        echo ""
        echo "SKIP: two-hop route ($SECOND_POOL exists but is pre-threshold)"
    fi
else
    echo ""
    echo "SKIP: two-hop route (needs a second threshold-crossed pool in commit_pools.txt)"
fi

# ---- Summary ---------------------------------------------------------------
echo ""
echo "==================================================="
echo "lifecycle test complete: $PASS passed, $FAIL failed"
echo "==================================================="
if [ "$FAIL" -gt 0 ]; then
    printf 'failed: %s\n' "${FAILED_STEPS[@]}" >&2
    exit 1
fi
