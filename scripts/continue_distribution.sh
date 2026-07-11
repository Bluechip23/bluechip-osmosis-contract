#!/usr/bin/env bash
# =====================================================================
# continue_distribution.sh — flush a pool's post-threshold committer
#                            payout ledger
# =====================================================================
# usage: scripts/continue_distribution.sh <pool_addr> [max-batches]
#
# A threshold cross pays nobody in the crossing tx — recipients are
# flushed in gas-budgeted batches (<=40/tx) by permissionless
# ContinueDistribution calls until the ledger drains. In production the
# keeper (keepers/, `npm run distribution-keeper`) does this; this
# script is the manual/testnet equivalent.
#
# Loops until DistributionState returns null (no active distribution)
# or max-batches (default 25) is hit.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

POOL_ADDR="${1:-}"
MAX_BATCHES="${2:-25}"
if [ -z "$POOL_ADDR" ]; then
    echo "usage: $0 <pool_addr> [max-batches]" >&2
    exit 1
fi

for (( i=1; i<=MAX_BATCHES; i++ )); do
    STATE="$(query_smart "$POOL_ADDR" '{"distribution_state":{}}')"
    if [ "$STATE" = "null" ] || [ -z "$STATE" ]; then
        echo "no active distribution — ledger fully drained"
        exit 0
    fi
    if echo "$STATE" | jq -e '.is_stalled == true' >/dev/null 2>&1; then
        echo "WARNING: distribution reports is_stalled=true (24h timeout hit)." >&2
        echo "         Admin recovery: RecoverPoolStuckStates via the factory;" >&2
        echo "         after 7 days anyone may SelfRecoverDistribution." >&2
    fi
    echo "[batch $i] distribution active — sending continue_distribution"
    RESULT="$(submit_tx wasm execute "$POOL_ADDR" '{"continue_distribution":{}}')"
    echo "          tx $(echo "$RESULT" | jq -r '.txhash')"
done

echo "hit max-batches=$MAX_BATCHES with distribution still active:" >&2
query_smart "$POOL_ADDR" '{"distribution_state":{}}' >&2
exit 1
