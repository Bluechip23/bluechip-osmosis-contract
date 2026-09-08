#!/usr/bin/env bash
# =====================================================================
# status.sh — health overview of a deployed stack (and optionally one pool)
# =====================================================================
# usage: scripts/status.sh [pool_addr]
#
# Without arguments: factory config, the live x/twap pricing probe
# (the single best health signal — commits fail closed through this
# exact path), router config, and the factory's pool registry.
#
# With a pool address: adds that pool's commit status, reserves,
# distribution state, and pending factory-notify flag — the same
# fields the RUNBOOK says to alert on.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
require_state

POOL_ADDR="${1:-}"

echo "chain:    $CHAIN_ID via $NODE"
echo "factory:  $FACTORY_ADDR"
echo "router:   ${ROUTER_ADDR:-<not deployed>}"
echo ""

echo "=== factory config ==="
query_smart "$FACTORY_ADDR" '{"factory":{}}' | jq '{
    bluechip_denom:                .factory.bluechip_denom,
    pricing_pool_id:               .factory.pricing_pool_id,
    fee_quote_denom:               .factory.fee_quote_denom,
    commit_threshold_limit_native: .factory.commit_threshold_limit_native,
    gamm_pool_creation_fee:        .factory.gamm_pool_creation_fee,
    pool_creation_fee:             .factory.pool_creation_fee,
    emergency_withdraw_delay_seconds: .factory.emergency_withdraw_delay_seconds,
    bluechip_wallet_address:       .factory.bluechip_wallet_address
}' 2>/dev/null || echo "(query failed)"

echo ""
echo "=== fee-route probe (CommitContext) ==="
PROBE="$(query_smart "$FACTORY_ADDR" \
    '{"pool_factory_query":{"commit_context":{}}}')"
if echo "$PROBE" | jq -e '.bluechip_wallet' >/dev/null 2>&1; then
    echo "$PROBE" | jq '{gamm_pool_creation_fee, fee_swap_budget_native}'
    echo "OK — crossings can budget the gamm creation fee"
else
    echo "FAILING — pre-threshold commits fail closed until this works: $PROBE"
fi

if [ -n "${ROUTER_ADDR:-}" ]; then
    echo ""
    echo "=== router config ==="
    query_smart "$ROUTER_ADDR" '{"config":{}}' | jq . 2>/dev/null || echo "(query failed)"
fi

echo ""
echo "=== pool registry (first page) ==="
query_smart "$FACTORY_ADDR" '{"pools":{"start_after":null,"limit":30}}' \
    | jq . 2>/dev/null || echo "(query failed)"

if [ -n "$POOL_ADDR" ]; then
    echo ""
    echo "==================================================="
    echo "pool: $POOL_ADDR"
    echo "==================================================="
    echo "--- commit status ---"
    query_smart "$POOL_ADDR" '{"is_fully_commited":{}}' | jq . 2>/dev/null || true
    echo "--- pool state (reserves / liquidity) ---"
    query_smart "$POOL_ADDR" '{"pool_state":{}}' | jq . 2>/dev/null || true
    echo "--- distribution state (alert on is_stalled) ---"
    query_smart "$POOL_ADDR" '{"distribution_state":{}}' | jq . 2>/dev/null || true
    echo "--- factory notify status (alert on pending) ---"
    query_smart "$POOL_ADDR" '{"factory_notify_status":{}}' | jq . 2>/dev/null || true
    echo "--- paused? ---"
    query_smart "$POOL_ADDR" '{"is_paused":{}}' | jq . 2>/dev/null || true
fi
