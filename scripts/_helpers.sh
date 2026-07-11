# shellcheck shell=bash
# =====================================================================
# Shared helpers for the Osmosis deploy + test scripts.
# =====================================================================
# Source AFTER defining SCRIPT_DIR, from any script:
#
#   REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
#   source "$REPO_ROOT/scripts/_helpers.sh"
#
# Reads the env file named by $ENV_FILE (default: osmo_testnet.env at
# the repo root), which must define:
#   CHAIN_ID, NODE, KEYRING, FROM, GAS_PRICES, GAS_ADJUSTMENT,
#   NATIVE_DENOM, STATE_FILE, ARTIFACTS
#
# Exports (functions):
#   ensure_tools                              asserts osmosisd/jq exist
#   ensure_key                                asserts $FROM key exists; sets $ADDR
#   submit_tx <subcommand-and-args>           tx-result JSON to stdout
#   query_json <query-args>                   query JSON to stdout
#   query_smart <contract> <msg_json>         response JSON to stdout
#   extract_attr <tx_json> <type> <key>       first matching attr value
#   require_state                             asserts deploy ran, sources state file
#
# Conventions:
#   - All status / error messages go to stderr; only the requested
#     value goes to stdout. Safe to capture function output via $(...).
#   - submit_tx polls for inclusion (~60s) and fails loudly on any
#     non-zero tx code; raw_log is printed on failure.
#   - osmosisd v29 (Cosmos-SDK v0.50) routes JSON output to stderr in
#     non-TTY contexts (subshell capture, pipes). Every capture below
#     merges the streams (2>&1) and isolates the JSON line so parsing
#     works regardless of which stream osmosisd picked.
# =====================================================================

if [ -z "${REPO_ROOT:-}" ]; then
    REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi

ENV_FILE="${ENV_FILE:-$REPO_ROOT/osmo_testnet.env}"
# Allow a bare filename ("osmo_testnet.env") as well as a path.
if [ ! -f "$ENV_FILE" ] && [ -f "$REPO_ROOT/$ENV_FILE" ]; then
    ENV_FILE="$REPO_ROOT/$ENV_FILE"
fi
if [ ! -f "$ENV_FILE" ]; then
    echo "error: env file not found: $ENV_FILE" >&2
    exit 1
fi
# shellcheck disable=SC1090
source "$ENV_FILE"

__TX_FLAGS=(
    --chain-id "$CHAIN_ID"
    --node "$NODE"
    --keyring-backend "$KEYRING"
    --from "$FROM"
    --gas auto
    --gas-adjustment "$GAS_ADJUSTMENT"
    --gas-prices "$GAS_PRICES"
    -y -o json
)

ensure_tools() {
    local cmd
    for cmd in osmosisd jq; do
        command -v "$cmd" >/dev/null \
            || { echo "error: $cmd not found on PATH" >&2; exit 1; }
    done
}

ensure_key() {
    if ! osmosisd keys show "$FROM" --keyring-backend "$KEYRING" >/dev/null 2>&1; then
        echo "error: key '$FROM' not found in keyring '$KEYRING'" >&2
        echo "       create one with: osmosisd keys add $FROM --keyring-backend $KEYRING" >&2
        echo "       fund it via:     https://faucet.testnet.osmosis.zone/" >&2
        exit 1
    fi
    ADDR="$(osmosisd keys show "$FROM" -a --keyring-backend "$KEYRING")"
    export ADDR
}

query_json() {
    osmosisd query "$@" --node "$NODE" -o json 2>&1
}

submit_tx() {
    local raw
    if ! raw="$(osmosisd tx "$@" "${__TX_FLAGS[@]}" 2>&1)"; then
        echo "error: tx submit (mempool admission) failed for: $*" >&2
        echo "--- osmosisd output ---" >&2
        echo "$raw" >&2
        echo "-----------------------" >&2
        return 1
    fi
    # osmosisd prints a "gas estimate: N" line before the JSON response;
    # with 2>&1 both land in $raw. Walk lines and keep the last that
    # looks like a JSON object. awk avoids subshell quirks (set -u)
    # seen with `grep '^{' | tail -n 1` when called inside $(...).
    local json
    json="$(printf '%s\n' "$raw" | awk '/^\{.*\}$/ {last=$0} END {print last}')"
    if [ -z "$json" ]; then
        echo "error: tx submit returned no JSON. raw output:" >&2
        echo "$raw" >&2
        return 1
    fi
    # CheckTx rejection: response carries height="0" and a non-zero code
    # (insufficient fee, contract revert at simulate, etc). Surface
    # raw_log so the operator sees *why*.
    local check_code
    check_code="$(echo "$json" | jq -r '.code // 0' 2>/dev/null || echo 0)"
    if [ "$check_code" != "0" ]; then
        echo "error: tx rejected at CheckTx with code $check_code for: $*" >&2
        echo "$json" | jq -r '.raw_log' 2>/dev/null >&2 || echo "$json" >&2
        return 1
    fi
    local tx_hash
    tx_hash="$(echo "$json" | jq -r '.txhash // empty')"
    if [ -z "$tx_hash" ]; then
        echo "error: tx submit returned no hash. raw output:" >&2
        echo "$raw" >&2
        return 1
    fi
    # Poll for inclusion. ~5s block time + indexing latency → try ~60s.
    local i result code
    for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
        sleep 5
        if result="$(query_json tx "$tx_hash" 2>/dev/null)" \
            && [ -n "$(printf '%s\n' "$result" | awk '/^\{/ {print; exit}')" ]; then
            code="$(echo "$result" | jq -r '.code // 0' 2>/dev/null || echo 0)"
            if [ "$code" != "0" ]; then
                echo "error: tx $tx_hash failed with code $code" >&2
                echo "$result" | jq -r '.raw_log' 2>/dev/null >&2 || echo "$result" >&2
                return 1
            fi
            echo "$result"
            return 0
        fi
    done
    echo "error: tx $tx_hash not indexed after 60s. check $NODE manually." >&2
    return 1
}

query_smart() {
    local contract="$1" msg="$2"
    local raw
    raw="$(osmosisd query wasm contract-state smart "$contract" "$msg" \
        --node "$NODE" -o json 2>&1)"
    # Newer osmosisd wraps responses in {data: ...}; older versions
    # return the response directly. Strip the wrapper whenever the key
    # is present — including when data is legitimately `null` (an
    # Option<T> query like DistributionState after completion), which
    # the previous `.data // empty` check misread as "no wrapper" and
    # passed through as `{"data":null}`, breaking callers' null checks.
    if echo "$raw" | jq -e 'type == "object" and has("data")' >/dev/null 2>&1; then
        echo "$raw" | jq -c '.data'
    else
        echo "$raw"
    fi
}

extract_attr() {
    local json="$1" type="$2" key="$3"
    echo "$json" | jq -r --arg t "$type" --arg k "$key" '
        [ .events[] | select(.type == $t) | .attributes[]
        | select(.key == $k) | .value ] | first // empty'
}

require_state() {
    local state_path="$REPO_ROOT/$STATE_FILE"
    if [ ! -f "$state_path" ]; then
        echo "error: $STATE_FILE not found at $REPO_ROOT — run ./deploy_osmosis.sh first" >&2
        exit 1
    fi
    # shellcheck disable=SC1090
    source "$state_path"
    if [ -z "${FACTORY_ADDR:-}" ]; then
        echo "error: FACTORY_ADDR not set in $STATE_FILE — deploy incomplete" >&2
        exit 1
    fi
}
