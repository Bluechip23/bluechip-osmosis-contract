#!/usr/bin/env bash
# LP lifecycle on one crossed pool's GAMM pool:
#   alice provides liquidity (MsgJoinPool) -> swapper generates round-trip
#   swap volume (0.3% fee accrues to LPs) -> alice removes liquidity
#   (MsgExitPool, shares burned) and collects the grown reserves.
# usage: run_lp.sh <pool_addr> <symbol_lower> <lp_native_micro>
export ENV_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/osmo_liverun.env"
source "$(dirname "${BASH_SOURCE[0]}")/liverun_lib.sh"

POOL="$1"; SYMLC="$2"; LPAMT="${3:-5000000}"
DENOM="factory/$POOL/$SYMLC"
GAMM="$(poolq "$POOL" '{"native_pool_id":{}}' | jq -r '.pool_id')"
record "----- LP on $SYMLC (pool $POOL, gamm $GAMM) -----" ""

gamm_reserves() { osmosisd query gamm pool "$GAMM" --node "$NODE" -o json 2>/dev/null \
    | jq -rc '{shares:.pool.total_shares.amount, assets:[.pool.pool_assets[].token]}'; }

record "$SYMLC gamm reserves (pre-LP)" "$(gamm_reserves)"

# 1. alice provides liquidity (two-sided join; alice holds creator tokens from
#    the creator reward + claimed excess)
alice_osmo0="$(bal_uosmo "$(addr_of alice)")"; alice_tok0="$(bal_denom "$(addr_of alice)" "$DENOM")"
ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/liquidity.sh" deposit "$POOL" "$LPAMT" >/tmp/lp.out 2>&1 || { log "deposit failed"; cat /tmp/lp.out >&2; }
record "$SYMLC LP deposit tx" "$(grep -oE '[A-F0-9]{64}' /tmp/lp.out | head -1)"
shares="$(ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/liquidity.sh" shares "$POOL" 2>/dev/null | grep -oE '[0-9]+' | tail -1)"
record "$SYMLC alice LP shares received" "$shares"
# Snapshot reserves right after the deposit (BEFORE any swap). Shares stay
# constant through the swaps, so any reserve growth below is pure swap fees.
record "$SYMLC gamm reserves (post-deposit, pre-swap)" "$(gamm_reserves)"

# 2. swapper generates round-trip volume DIRECTLY on the GAMM pool via
#    poolmanager (native swap, 0.3% swap_fee accrues to LPs). Native-module
#    swaps have no contract belief-price gate — that gate is exercised
#    separately in the safety-protocol section.
for round in 1 2; do
    tok_before="$(bal_denom "$(addr_of swapper)" "$DENOM")"
    r="$(submit_as swapper poolmanager swap-exact-amount-in 200000uosmo 1 \
        --swap-route-pool-ids "$GAMM" --swap-route-denoms "$DENOM")" \
        && record "$SYMLC swap$round buy 0.2 OSMO->tok (gamm)" "$(txhash_of "$r")" || log "$SYMLC buy$round failed"
    tok_after="$(bal_denom "$(addr_of swapper)" "$DENOM")"
    got=$(( ${tok_after:-0} - ${tok_before:-0} ))
    if [ "$got" -gt 0 ]; then
        r="$(submit_as swapper poolmanager swap-exact-amount-in "${got}${DENOM}" 1 \
            --swap-route-pool-ids "$GAMM" --swap-route-denoms uosmo)" \
            && record "$SYMLC swap$round sell tok->OSMO (gamm)" "$(txhash_of "$r")" || log "$SYMLC sell$round failed"
    fi
done

record "$SYMLC gamm reserves (post-swaps, fees accrued)" "$(gamm_reserves)"

# 3. alice removes ALL liquidity (MsgExitPool burns shares, returns grown reserves)
ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/liquidity.sh" remove "$POOL" >/tmp/lpr.out 2>&1 || { log "remove failed"; cat /tmp/lpr.out >&2; }
record "$SYMLC LP remove(exit) tx" "$(grep -oE '[A-F0-9]{64}' /tmp/lpr.out | head -1)"
record "$SYMLC alice LP shares after exit (expect 0)" "$(ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/liquidity.sh" shares "$POOL" 2>/dev/null | grep -oE '[0-9]+' | tail -1)"
alice_osmo1="$(bal_uosmo "$(addr_of alice)")"; alice_tok1="$(bal_denom "$(addr_of alice)" "$DENOM")"
record "$SYMLC alice OSMO deposit->exit" "$alice_osmo0 -> $alice_osmo1"
record "$SYMLC alice TOK  deposit->exit" "$alice_tok0 -> $alice_tok1"
record "$SYMLC LP DONE" ""
