#!/usr/bin/env bash
# Safety-protocol triggers on the live stack (everything on-chain except the
# large-OSMO liquidity breaker). Each check asserts the expected accept/reject.
export ENV_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/osmo_liverun.env"
source "$(dirname "${BASH_SOURCE[0]}")/liverun_lib.sh"
require_state   # FACTORY_ADDR, ROUTER_ADDR

CROSSED="$1"        # a crossed pool addr (for belief gate / no-double-cross)
CROSSED_SYMLC="$2"  # its symbol lower
EW_POOL="$3"        # sacrificial crossed pool addr for emergency-withdraw
EW_POOL_ID="$4"     # its numeric pool_id (registry id, not gamm id)

expect_reject() { # <label> <substr> ; reads $? and stderr capture file /tmp/sf.err
    if grep -qiE "$2" /tmp/sf.err; then record "REJECTED (as expected): $1" "matched '$2'"; else record "!! UNEXPECTED for $1" "$(tail -c 200 /tmp/sf.err)"; fi
}

section() { { echo ""; echo "===== SAFETY: $1 ====="; } >>"$RESULTS"; log "SAFETY: $1"; }

# ---------------------------------------------------------------
section "belief-price gate (F-1) on crossed pool $CROSSED_SYMLC"
# null belief, direct caller -> rejected
set_price_fresh 10000000 15
DENOM="factory/$CROSSED/$CROSSED_SYMLC"
submit_as swapper wasm execute "$CROSSED" '{"simple_swap":{"offer_asset":{"info":{"bluechip":{"denom":"uosmo"}},"amount":"100000"},"belief_price":null,"max_spread":"0.05","to":null,"transaction_deadline":null}}' --amount 100000uosmo 2>/tmp/sf.err
expect_reject "null-belief direct SimpleSwap" "belief_price is required|belief"
# valid belief price (loose) -> succeeds, and generates a 0.3% LP fee
r="$(submit_as swapper wasm execute "$CROSSED" '{"simple_swap":{"offer_asset":{"info":{"bluechip":{"denom":"uosmo"}},"amount":"100000"},"belief_price":"1.0","max_spread":"0.05","to":null,"transaction_deadline":null}}' --amount 100000uosmo)" \
    && record "ACCEPTED: belief-priced direct SimpleSwap" "$(txhash_of "$r")" || record "!! belief-priced swap failed" "$(tail -c 200 /tmp/sf.err 2>/dev/null)"

# ---------------------------------------------------------------
section "RATE plausibility ceiling (audit fix) + staleness (fail-closed) via a fresh pre-threshold pool"
sleep 32
ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/create_commit_pool.sh" "Safety Pool" "SAFE" >/tmp/cp.out 2>&1 || { cat /tmp/cp.out; }
SAFE="$(awk '$5=="SAFE"{print $2}' "$REPO_ROOT/commit_pools.txt" | tail -1)"
record "SAFE pool_addr" "$SAFE"

# (a) RATE band: set mock price to $200/OSMO (rate 200_000_000 > $100 ceiling) -> commit rejected
msg="$(jq -nc '{set_price:{price_id:"'"$FEED"'",price:200000000,expo:-6,conf:0}}')"
submit_as alice wasm execute "$MOCK_PYTH" "$msg" >/dev/null 2>&1; sleep 15
commit "$SAFE" bob 1000000 2>/tmp/sf.err
expect_reject "commit at \$200/OSMO (over \$100 plausibility ceiling)" "plausibility ceiling|InvalidOraclePrice|ceiling"

# (b) staleness: set a price with an OLD publish_time -> commit rejected stale
msg="$(jq -nc '{set_price_at:{price_id:"'"$FEED"'",price:10000000,expo:-6,conf:0,publish_time:1700000000}}')"
submit_as alice wasm execute "$MOCK_PYTH" "$msg" >/dev/null 2>&1; sleep 3
commit "$SAFE" bob 1000000 2>/tmp/sf.err
expect_reject "commit against a stale Pyth price" "stale|InvalidOraclePrice|age"

# (c) min-commit: fresh valid price, commit worth < \$5 -> rejected
set_price_fresh 10000000 15
commit "$SAFE" bob 400000 2>/tmp/sf.err   # 0.4 OSMO = \$4 < \$5 min
expect_reject "sub-\$5 pre-threshold commit" "minimum|min_commit|too small|below"

# ---------------------------------------------------------------
section "no-double-cross (post-threshold commit on already-crossed $CROSSED_SYMLC)"
set_price_fresh 10000000 15
# a valid post-threshold commit needs belief_price; a 2nd crossing must NOT occur
r="$(submit_as bob wasm execute "$CROSSED" '{"commit":{"asset":{"info":{"bluechip":{"denom":"uosmo"}},"amount":"500000"},"transaction_deadline":null,"belief_price":"1.0","max_spread":"0.05"}}' --amount 500000uosmo 2>/tmp/sf.err)" \
    && record "post-threshold commit accepted (swap-like, no re-cross)" "$(txhash_of "$r")" \
    || expect_reject "post-threshold commit" "belief_price|threshold|already"
record "$CROSSED_SYMLC still single gamm pool (no re-cross)" "$(poolq "$CROSSED" '{"native_pool_id":{}}')"

# ---------------------------------------------------------------
section "router registration timelock (Propose -> early Apply rejected -> wait 120s -> Apply)"
# ProposeRouter
submit_as alice wasm execute "$FACTORY_ADDR" "$(jq -nc --arg r "$ROUTER_ADDR" '{propose_router:{router:$r}}')" >/tmp/sf.err 2>&1 \
    && record "ProposeRouter" "ok" || { record "ProposeRouter variant?" "$(tail -c 300 /tmp/sf.err)"; }
# early apply -> rejected (timelock)
submit_as alice wasm execute "$FACTORY_ADDR" '{"apply_router":{}}' 2>/tmp/sf.err
expect_reject "ApplyRouter before 120s timelock" "timelock|not.*expired|too early|TimelockNotExpired"
record "router timelock note" "waiting 125s then applying"
sleep 125
r="$(submit_as alice wasm execute "$FACTORY_ADDR" '{"apply_router":{}}')" \
    && record "ApplyRouter after timelock" "$(txhash_of "$r")" || record "!! ApplyRouter failed" "$(tail -c 200 /tmp/sf.err)"
record "RegisteredRouter" "$(query_smart "$FACTORY_ADDR" '{"pool_factory_query":{"registered_router":{}}}' 2>/dev/null)"

# ---------------------------------------------------------------
section "emergency withdraw arc (60s timelock) on sacrificial pool id=$EW_POOL_ID"
# Pools reject direct EmergencyWithdraw; admin relays via the factory.
record "EW gamm reserves before" "$(osmosisd query gamm pool "$(poolq "$EW_POOL" '{"native_pool_id":{}}' | jq -r .pool_id)" --node "$NODE" -o json 2>/dev/null | jq -rc '{shares:.pool.total_shares.amount,assets:[.pool.pool_assets[].token]}')"
bw_before="$(bal_uosmo "$(addr_of alice)")"
# Phase 1: initiate + pause
r="$(submit_as alice wasm execute "$FACTORY_ADDR" "$(jq -nc --argjson id "$EW_POOL_ID" '{emergency_withdraw_pool:{pool_id:$id}}')")" \
    && record "EW phase1 initiate+pause" "$(txhash_of "$r")" || record "!! EW initiate failed" "$(tail -c 200 /tmp/sf.err 2>/dev/null)"
record "EW pool paused?" "$(poolq "$EW_POOL" '{"is_paused":{}}' 2>/dev/null)"
# a commit while paused -> rejected
set_price_fresh 10000000 15
commit "$EW_POOL" bob 1000000 2>/tmp/sf.err
expect_reject "commit while pool paused (emergency)" "paused|halt|Paused|emergency"
# early drain (before 60s) -> rejected
submit_as alice wasm execute "$FACTORY_ADDR" "$(jq -nc --argjson id "$EW_POOL_ID" '{emergency_withdraw_pool:{pool_id:$id}}')" 2>/tmp/sf.err
expect_reject "EW phase2 drain before 60s timelock" "timelock|not.*expired|too early|TimelockNotExpired"
sleep 62
# Phase 2: drain -> reserves to bluechip wallet (alice)
r="$(submit_as alice wasm execute "$FACTORY_ADDR" "$(jq -nc --argjson id "$EW_POOL_ID" '{emergency_withdraw_pool:{pool_id:$id}}')")" \
    && record "EW phase2 drain (after 60s)" "$(txhash_of "$r")" || record "!! EW drain failed" "$(tail -c 200 /tmp/sf.err 2>/dev/null)"
record "EW alice(bluechip wallet) native before->after" "$bw_before -> $(bal_uosmo "$(addr_of alice)")"
record "EW gamm reserves after drain" "$(osmosisd query gamm pool "$(poolq "$EW_POOL" '{"native_pool_id":{}}' | jq -r .pool_id)" --node "$NODE" -o json 2>/dev/null | jq -rc '{shares:.pool.total_shares.amount,assets:[.pool.pool_assets[].token]}')"

record "SAFETY CHECKS COMPLETE" "$(date -u +%H:%M)"
