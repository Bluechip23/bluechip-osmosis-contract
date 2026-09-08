#!/usr/bin/env bash
# =====================================================================
# cross_threshold.sh — commit enough OSMO into a commit pool to cross
#                      its threshold (seeds the AMM + mints the
#                      creator-token supply + starts distribution)
# =====================================================================
# usage: scripts/cross_threshold.sh <pool_addr> [native-amount-micro]
#
#   <pool_addr>            Commit pool address (commit_pools.txt col 2).
#   [native-amount-micro]  Optional. NATIVE_DENOM to commit in one tx,
#                          base units (6 decimals). When omitted the
#                          script auto-sizes: the remaining gap from
#                          IsFullyCommited IS the amount to attach —
#                          the threshold is native-denominated.
#
# How the crossing works on-chain:
#   1. A commit's value toward the threshold IS its attached OSMO
#      (no conversion, no oracle).
#   2. Cumulative OSMO raised is compared against the pool's pinned
#      commit_threshold_limit_native.
#   3. The crossing commit triggers the threshold payout — mints the
#      creator-token supply, seeds the AMM reserves, and queues the
#      committer distribution ledger. NOBODY is paid in the crossing
#      tx: run scripts/continue_distribution.sh (or the keeper) to
#      flush the gas-budgeted batches.
#
# Constraints honored:
#   - the crossing commit must meet the pre-threshold minimum
#     (production: 115 OSMO)
#   - one commit per wallet per 13s (DEFAULT_SWAP_RATE_LIMIT_SECS)
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

POOL_ADDR="${1:-}"
AMOUNT="${2:-}"
if [ -z "$POOL_ADDR" ]; then
    echo "usage: $0 <pool_addr> [native-amount-micro]" >&2
    echo "" >&2
    if [ -f "$REPO_ROOT/commit_pools.txt" ]; then
        echo "known commit pools (from commit_pools.txt):" >&2
        awk -F '\t' '{printf "  pool_id=%s addr=%s symbol=%s\n", $1, $2, $5}' \
            "$REPO_ROOT/commit_pools.txt" >&2
    else
        echo "(commit_pools.txt not found — run scripts/create_commit_pool.sh first)" >&2
    fi
    exit 1
fi

# CommitStatus::FullyCommitted is a unit variant — it serializes as the
# bare string "fully_committed"; guard the type before indexing.
is_fully_committed() {
    echo "$1" | jq -e '
        (type == "string" and . == "fully_committed")
        or (type == "object" and has("fully_committed"))' >/dev/null 2>&1
}

# ---- Read the pool's commit progress --------------------------------
STATUS="$(query_smart "$POOL_ADDR" '{"is_fully_commited":{}}')"
if is_fully_committed "$STATUS"; then
    echo "pool is already fully committed — nothing to do"
    exit 0
fi
RAISED="$(echo "$STATUS" | jq -r '.in_progress.raised // "0"' 2>/dev/null || echo 0)"
TARGET="$(echo "$STATUS" | jq -r '.in_progress.target // empty' 2>/dev/null || true)"
if [ -z "$TARGET" ]; then
    echo "error: unexpected IsFullyCommited response: $STATUS" >&2
    exit 1
fi

REMAINING=$(( TARGET - RAISED ))
echo "pool:            $POOL_ADDR"
echo "raised:          $(awk -v u="$RAISED" 'BEGIN{printf "%.2f", u/1e6}') / $(awk -v u="$TARGET" 'BEGIN{printf "%.2f", u/1e6}') OSMO"

if [ -z "$AMOUNT" ]; then
    # The threshold is native-denominated: the remaining gap IS the
    # amount to attach — value toward the threshold = attached OSMO.
    AMOUNT="$REMAINING"
fi
echo "committing:      $AMOUNT $NATIVE_DENOM"
echo ""

COMMIT_MSG="$(jq -nc \
    --arg denom "$NATIVE_DENOM" \
    --arg amt   "$AMOUNT" \
    '{commit:{
        asset:{
            info:{bluechip:{denom:$denom}},
            amount:$amt
        },
        transaction_deadline:null,
        belief_price:null,
        max_spread:null
    }}')"

RESULT="$(submit_tx wasm execute "$POOL_ADDR" "$COMMIT_MSG" \
    --amount "${AMOUNT}${NATIVE_DENOM}")"
echo "OK — tx $(echo "$RESULT" | jq -r '.txhash')"

# ---- Report crossing state -------------------------------------------
echo ""
echo "=== pool.is_fully_commited ==="
AFTER="$(query_smart "$POOL_ADDR" '{"is_fully_commited":{}}')"
echo "$AFTER"

if is_fully_committed "$AFTER"; then
    echo ""
    echo "THRESHOLD CROSSED — the AMM is seeded and the committer"
    echo "distribution ledger is queued."
    echo ""
    echo "NEXT:"
    echo "  scripts/continue_distribution.sh $POOL_ADDR   # flush payout batches"
    echo "  scripts/swap.sh $POOL_ADDR native 1000000     # trade against the AMM"
    echo "  scripts/status.sh $POOL_ADDR"
else
    echo ""
    echo "threshold not crossed yet — commit more:"
    echo "  scripts/cross_threshold.sh $POOL_ADDR"
    echo "(remember the 13s per-wallet commit rate limit)"
fi
