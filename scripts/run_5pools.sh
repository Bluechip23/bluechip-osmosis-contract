#!/usr/bin/env bash
# 5-pool live run: create -> committer distribution -> cross -> distribute
# -> verify shares -> claim creator excess. Logs every txhash to RESULTS.
# Committers: space-separated "key:uosmo" list; the last one crosses.
export ENV_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/osmo_liverun.env"
source "$(dirname "${BASH_SOURCE[0]}")/liverun_lib.sh"

DENOM_OF() { echo "factory/$1/$2"; }   # pool_addr, symbol_lower

lifecycle() {
    local sym="$1" name="$2" overshoot="$3" committers="$4"
    local symlc; symlc="$(echo "$sym" | tr '[:upper:]' '[:lower:]')"
    record "===== POOL $sym ($name) overshoot=$overshoot =====" ""
    # cooldown safety: alice's per-address commit-pool create is rate-limited
    # (30s on the integration build); space creates out regardless of lifecycle time.
    sleep 32
    # 1. create (retry on transient network failure; safe because a failed
    # broadcast creates no pool)
    local ca
    for ca in 1 2 3 4; do
        ENV_FILE="$ENV_FILE" "$REPO_ROOT/scripts/create_commit_pool.sh" "$name" "$sym" >/tmp/cp.out 2>&1 && break
        if grep -qiE "post failed|EOF|connection re|timeout" /tmp/cp.out; then
            log "$sym create network blip (attempt $ca), retry in 10s"; sleep 10; continue
        fi
        log "create failed ($sym)"; cat /tmp/cp.out >&2; return 1
    done
    local pool; pool="$(awk -v s="$sym" '$5==s {print $2}' "$REPO_ROOT/commit_pools.txt" | tail -1)"
    [ -z "$pool" ] && { log "no pool addr for $sym"; return 1; }
    record "$sym pool_addr" "$pool"
    local denom; denom="$(DENOM_OF "$pool" "$symlc")"
    # 2. fresh price for the commit window
    set_price_fresh 10000000 15
    # 3. commits in order (last crosses)
    local pair key amt r
    for pair in $committers; do
        key="${pair%%:*}"; amt="${pair##*:}"
        local before_native; before_native="$(bal_uosmo "$(addr_of "$key")")"
        r="$(commit "$pool" "$key" "$amt")" || { log "$sym commit $key $amt FAILED"; return 1; }
        record "$sym commit $key ${amt}uosmo(\$$((amt/100000)).x)" "$(txhash_of "$r")"
        local after_native; after_native="$(bal_uosmo "$(addr_of "$key")")"
        log "    $key native $before_native -> $after_native"
        sleep 2
    done
    # 4. verify crossing
    local fc np
    fc="$(poolq "$pool" '{"is_fully_commited":{}}' 2>/dev/null)"
    np="$(poolq "$pool" '{"native_pool_id":{}}' 2>/dev/null)"
    record "$sym crossed? is_fully_commited" "$fc"
    record "$sym gamm pool" "$np"
    # 5. distribution
    "$REPO_ROOT/scripts/continue_distribution.sh" "$pool" >/tmp/cd.out 2>&1 || true
    grep -oE "tx [A-F0-9]{64}" /tmp/cd.out | while read -r _ h; do record "$sym distribution batch" "$h"; done
    local ds; ds="$(poolq "$pool" '{"distribution_state":{}}' 2>/dev/null)"
    record "$sym distribution_state (null=done)" "$ds"
    # 6. committer creator-token shares
    for pair in $committers; do
        key="${pair%%:*}"
        record "$sym share $key ($denom)" "$(bal_denom "$(addr_of "$key")" "$denom")"
    done
    # 7. claim creator excess (alice = creator); lock_days=0 => immediate
    local ce alice_before alice_after
    ce="$(poolq "$pool" '{"creator_earnings":{}}' 2>/dev/null)"
    record "$sym creator_earnings(pre-claim)" "$ce"
    alice_before="$(bal_uosmo "$(addr_of alice)")"
    r="$(submit_as alice wasm execute "$pool" '{"claim_creator_excess_liquidity":{}}')" \
        && record "$sym ClaimCreatorExcessLiquidity" "$(txhash_of "$r")" \
        || record "$sym ClaimCreatorExcessLiquidity" "FAILED/none"
    alice_after="$(bal_uosmo "$(addr_of alice)")"
    record "$sym alice native +excess" "$alice_before -> $alice_after (creator-tok $(bal_denom "$(addr_of alice)" "$denom"))"
    ce="$(poolq "$pool" '{"creator_earnings":{}}' 2>/dev/null)"
    record "$sym creator_earnings(post-claim, excess should be null)" "$ce"
    record "$sym DONE" "$pool"
}

# ---- pools 2-5 (LPONE done inline as the smoke test) ----
lifecycle LPTWO  "Live Pool Two"   no  "bob:2500000 carol:7500000"                               # 25/75
lifecycle LPTRE  "Live Pool Three" no  "bob:3333334 carol:3333333 dave:3333333"                  # 1/3 each
lifecycle LPFOR  "Live Pool Four"  no  "bob:1000000 carol:3000000 dave:2000000 keeper:4000000"   # 10/30/20/40
lifecycle LPFIV  "Live Pool Five"  yes "bob:2000000 carol:2000000 dave:2000000 keeper:2000000 pusher:4000000"  # 20% each, 5th $40 overshoot

record "ALL POOLS COMPLETE" "$(date -u +%H:%M)"
