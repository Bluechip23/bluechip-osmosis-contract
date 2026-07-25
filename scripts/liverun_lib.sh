# shellcheck shell=bash
# =====================================================================
# liverun_lib.sh — helpers for the 5-pool live-run orchestration.
# Sourced by each step. Uses osmo_liverun.env + osmo_liverun.state.
# =====================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ENV_FILE="${ENV_FILE:-$REPO_ROOT/osmo_liverun.env}"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"   # sources ENV_FILE, gives query_smart/extract_attr/submit_tx

LCD="https://lcd.osmotest5.osmosis.zone"
# RPC failover pool (the public testnet RPCs blip intermittently).
RPCS=("https://rpc.testnet.osmosis.zone:443" "https://rpc.osmotest5.osmosis.zone:443")
RESULTS="$REPO_ROOT/liverun_results.log"
MOCK_PYTH="${PYTH_CONTRACT_ADDR}"
FEED="${PYTH_NATIVE_USD_FEED_ID}"

log() { printf '%s\n' "$*" >&2; }
record() { printf '%-40s %s\n' "$1" "$2" | tee -a "$RESULTS" >&2; }

addr_of() { osmosisd keys show "$1" -a --keyring-backend test 2>/dev/null; }

# uosmo balance via LCD (osmosisd rapid queries are flaky).
bal_uosmo() {
    curl -s --max-time 20 "$LCD/cosmos/bank/v1beta1/balances/$1/by_denom?denom=uosmo" 2>/dev/null \
        | python3 -c "import sys,json;print(json.load(sys.stdin).get('balance',{}).get('amount','0'))" 2>/dev/null
}
# balance of an arbitrary denom via LCD
bal_denom() {
    curl -s --max-time 20 "$LCD/cosmos/bank/v1beta1/balances/$1/by_denom?denom=$(python3 -c "import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1],safe=''))" "$2")" 2>/dev/null \
        | python3 -c "import sys,json;print(json.load(sys.stdin).get('balance',{}).get('amount','0'))" 2>/dev/null
}

# submit_as <keyname> <tx subcommand and args...>  -> prints tx-result JSON, polls to inclusion.
# Mirrors _helpers.sh submit_tx but with a per-tx --from key.
submit_as() {
    local key="$1"; shift
    local raw json code tx_hash i result attempt
    # Broadcast with retry on transient network errors (EOF / post failed /
    # connection reset) — the public testnet RPCs blip intermittently.
    raw=""
    for attempt in 1 2 3 4 5 6; do
        local node="${RPCS[$(( (attempt-1) % ${#RPCS[@]} ))]}"
        raw="$(osmosisd tx "$@" \
            --chain-id "$CHAIN_ID" --node "$node" --keyring-backend test --from "$key" \
            --gas auto --gas-adjustment 1.4 --gas-prices "$GAS_PRICES" -y -o json 2>&1)"
        if printf '%s\n' "$raw" | grep -qiE "post failed|EOF|connection re|timeout|context deadline|error trying to connect"; then
            log "submit_as($key): network blip on $node (attempt $attempt), rotating RPC in 6s"; sleep 6; continue
        fi
        break
    done
    if [ -z "$raw" ] || printf '%s\n' "$raw" | grep -qiE "post failed|EOF|connection re|error trying to connect"; then
        log "submit_as($key): mempool admission FAILED (network)"; printf '%s\n' "$raw" >&2; return 1; fi
    json="$(printf '%s\n' "$raw" | awk '/^\{.*\}$/ {last=$0} END {print last}')"
    [ -z "$json" ] && { log "submit_as($key): no JSON"; printf '%s\n' "$raw" >&2; return 1; }
    code="$(echo "$json" | jq -r '.code // 0' 2>/dev/null || echo 0)"
    if [ "$code" != "0" ]; then
        log "submit_as($key): CheckTx rejected code=$code"; echo "$json" | jq -r '.raw_log' >&2; return 2; fi
    tx_hash="$(echo "$json" | jq -r '.txhash // empty')"
    [ -z "$tx_hash" ] && { log "submit_as($key): no txhash"; return 1; }
    for i in $(seq 1 18); do
        sleep 4
        result="$(osmosisd query tx "$tx_hash" --node "${RPCS[$(( (i-1) % ${#RPCS[@]} ))]}" -o json 2>/dev/null)" || continue
        [ -z "$(printf '%s\n' "$result" | awk '/^\{/ {print; exit}')" ] && continue
        code="$(echo "$result" | jq -r '.code // 0' 2>/dev/null || echo 0)"
        if [ "$code" != "0" ]; then
            log "submit_as($key): tx $tx_hash FAILED code=$code"; echo "$result" | jq -r '.raw_log' >&2; return 3; fi
        printf '%s\n' "$result"; return 0
    done
    log "submit_as($key): tx $tx_hash not indexed in 60s"; return 1
}

# last txhash from a submit_as result JSON
txhash_of() { echo "$1" | jq -r '.txhash // empty'; }

# SetPrice on the mock, then wait so the price ages past MIN_PYTH_AGE (10s).
# $1 = price (micro, expo -6), default 10_000_000 ($10). $2 = wait secs (default 15).
set_price_fresh() {
    local price="${1:-10000000}" waits="${2:-15}"
    local msg r
    msg="$(python3 -c "import json,sys;print(json.dumps({'set_price':{'price_id':'$FEED','price':int(sys.argv[1]),'expo':-6,'conf':0}}))" "$price")"
    r="$(submit_as alice wasm execute "$MOCK_PYTH" "$msg")" || return 1
    log "set_price \$$(python3 -c "print($price/1e6)")/OSMO tx=$(txhash_of "$r"); aging ${waits}s"
    sleep "$waits"
}

# commit <pool_addr> <keyname> <uosmo_amount>   (pre-threshold; belief_price null)
# Wire shape must match cross_threshold.sh exactly: native TokenType tag is
# {"bluechip":{"denom":..}} and Commit has only 4 fields.
commit() {
    local pool="$1" key="$2" amt="$3"
    local msg
    msg="$(jq -nc --arg amt "$amt" '{commit:{asset:{info:{bluechip:{denom:"uosmo"}},amount:$amt},transaction_deadline:null,belief_price:null,max_spread:null}}')"
    submit_as "$key" wasm execute "$pool" "$msg" --amount "${amt}uosmo"
}

# query a pool smart msg, with RPC failover (osmosisd query blips too)
poolq() {
    local c="$1" m="$2" i node raw
    for i in 1 2 3 4 5 6; do
        node="${RPCS[$(( (i-1) % ${#RPCS[@]} ))]}"
        raw="$(osmosisd query wasm contract-state smart "$c" "$m" --node "$node" -o json 2>&1)"
        if printf '%s\n' "$raw" | grep -qiE "post failed|EOF|connection re|timeout|error trying to connect"; then
            sleep 5; continue
        fi
        if echo "$raw" | jq -e 'type=="object" and has("data")' >/dev/null 2>&1; then
            echo "$raw" | jq -c '.data'; return 0
        fi
        echo "$raw"; return 0
    done
    echo "QUERY_FAILED"; return 1
}
