#!/usr/bin/env bash
# =====================================================================
# create_commit_pool.sh — spin up a commit (creator) pool via factory
# =====================================================================
# usage: scripts/create_commit_pool.sh <name> <symbol>
#
#   <name>   3-50 printable ASCII chars  (e.g. "Alpha Creator")
#   <symbol> 3-12 chars A-Z + 0-9, must contain at least one letter
#            (e.g. "ALPHA")
#
# Sends factory.Create with only pool_token_info + token_info —
# everything else (commit threshold, fees, payout amounts, lock caps)
# is read from the factory's stored config. The CreatorToken slot must
# carry the factory's sentinel string; the pool registers its own
# TokenFactory denom (`factory/{pool_addr}/{symbol_lowercase}`) at
# instantiate and the factory rewrites the field.
#
# Pays the flat pool-creation fee (factory config `pool_creation_fee`,
# read live) in NATIVE_DENOM; surplus is refunded, zero disables. The
# x/gamm pool-creation fee is NOT collected here — the pool retains 1%
# commit fees toward it and settles against the live fee at crossing.
#
# Per-address rate limit (COMMIT_POOL_CREATE_RATE_LIMIT_SECONDS): one
# commit pool per address per hour on the prod factory build (30s on
# the integration build). To create several quickly, either deploy with
# FACTORY_WASM_FILE=factory-integration.wasm or rotate FROM keys.
#
# Side effects:
#   - Appends one line per created pool to commit_pools.txt:
#       <pool_id>\t<pool_addr>\t<creator_token_denom>\t-\t<symbol>
#     (col 4 held the position-NFT addr pre-migration; kept as "-" so
#     downstream column indices stay stable.) Downstream scripts
#     (cross_threshold.sh, swap.sh, route_swap.sh,
#     run_lifecycle_test.sh) iterate over this file.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/_helpers.sh"
ensure_tools
ensure_key
require_state

NAME="${1:-}"
SYMBOL="${2:-}"
if [ -z "$NAME" ] || [ -z "$SYMBOL" ]; then
    echo "usage: $0 <name> <symbol>" >&2
    echo "  example: $0 'Alpha Creator' ALPHA" >&2
    exit 1
fi

# Client-side validation matching factory's validate_creator_token_info —
# catches obvious mistakes before burning a tx.
NAME_LEN="${#NAME}"
if [ "$NAME_LEN" -lt 3 ] || [ "$NAME_LEN" -gt 50 ]; then
    echo "error: name must be 3-50 printable ASCII chars (got $NAME_LEN)" >&2
    exit 1
fi
# Mirrors factory validate_creator_token_info: uppercase A-Z + digits,
# at least one letter (pure-digit tickers are rejected on-chain). The
# old cw20-base no-digits restriction is gone with the TokenFactory
# migration — the subdenom is just the lowercased symbol.
if ! [[ "$SYMBOL" =~ ^[A-Z0-9]{3,12}$ ]] || ! [[ "$SYMBOL" =~ [A-Z] ]]; then
    echo "error: symbol must be 3-12 chars of A-Z/0-9 with at least one letter (got '$SYMBOL')" >&2
    exit 1
fi

# Read the live creation fee from the factory (admin-tunable, so don't
# trust the env file's snapshot).
CREATION_FEE="$(query_smart "$FACTORY_ADDR" '{"factory":{}}' \
    | jq -r '.factory.pool_creation_fee // empty' 2>/dev/null || true)"
CREATION_FEE="${CREATION_FEE:-${POOL_CREATION_FEE:-0}}"

echo "creating commit pool: name='$NAME' symbol='$SYMBOL'"
echo "factory:      $FACTORY_ADDR"
echo "creator:      $ADDR"
echo "creation fee: $CREATION_FEE $NATIVE_DENOM"
echo ""

# The CreatorToken sentinel is pinned by
# factory/src/execute/pool_lifecycle/create.rs::CREATOR_TOKEN_SENTINEL.
CREATE_MSG="$(jq -nc \
    --arg denom  "$NATIVE_DENOM" \
    --arg name   "$NAME" \
    --arg symbol "$SYMBOL" \
    '{create:{
        pool_msg:{
            pool_token_info:[
                {bluechip:{denom:$denom}},
                {creator_token:{denom:"WILL_BE_CREATED_BY_FACTORY"}}
            ]
        },
        token_info:{
            name:$name,
            symbol:$symbol,
            decimal:6
        }
    }}')"

if [ "$CREATION_FEE" != "0" ]; then
    CREATE_RESULT="$(submit_tx wasm execute "$FACTORY_ADDR" "$CREATE_MSG" \
        --amount "${CREATION_FEE}${NATIVE_DENOM}")"
else
    CREATE_RESULT="$(submit_tx wasm execute "$FACTORY_ADDR" "$CREATE_MSG")"
fi

POOL_ID="$(extract_attr "$CREATE_RESULT" wasm pool_id)"
if [ -z "$POOL_ID" ] || [ "$POOL_ID" = "null" ]; then
    echo "error: could not extract pool_id from create tx" >&2
    echo "$CREATE_RESULT" | jq '.events[] | select(.type=="wasm")' >&2
    exit 1
fi

# The factory's create reply emits pool_address; the pool's own
# instantiate emits token_denom (the TokenFactory denom it registered).
# Fall back to the instantiate event filtered by code_id / the
# deterministic factory/{pool}/{symbol} shape.
POOL_ADDR="$(extract_attr "$CREATE_RESULT" wasm pool_address)"
CREATOR_TOKEN_DENOM="$(extract_attr "$CREATE_RESULT" wasm token_denom)"

instantiated_by_code_id() {
    echo "$CREATE_RESULT" | jq -r --arg cid "$1" '
        [ .events[] | select(.type == "instantiate") |
          (.attributes | from_entries) |
          select(.code_id == $cid) | ._contract_address ] | first // empty'
}
[ -z "$POOL_ADDR" ] && POOL_ADDR="$(instantiated_by_code_id "$CREATOR_POOL_CODE_ID")"
if [ -z "$CREATOR_TOKEN_DENOM" ] && [ -n "$POOL_ADDR" ]; then
    CREATOR_TOKEN_DENOM="factory/${POOL_ADDR}/$(echo "$SYMBOL" | tr 'A-Z' 'a-z')"
fi

echo "pool_id:        $POOL_ID"
echo "pool address:   ${POOL_ADDR:-?}"
echo "creator denom:  ${CREATOR_TOKEN_DENOM:-?}"

if [ -z "$POOL_ADDR" ]; then
    echo "error: pool address missing from tx events — creation may still be" >&2
    echo "       in-flight; check: query_smart factory {\"pool_creation_status\":{\"pool_id\":$POOL_ID}}" >&2
    exit 1
fi

LOG_FILE="$REPO_ROOT/commit_pools.txt"
printf '%s\t%s\t%s\t%s\t%s\n' \
    "$POOL_ID" "$POOL_ADDR" "$CREATOR_TOKEN_DENOM" "-" "$SYMBOL" >> "$LOG_FILE"
echo ""
echo "appended entry to $LOG_FILE"

echo ""
echo "NEXT:"
echo "  scripts/cross_threshold.sh $POOL_ADDR    # commit past the USD threshold"
echo "  scripts/status.sh $POOL_ADDR             # watch pool state"
