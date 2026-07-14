#!/usr/bin/env bash
# =====================================================================
# make_storecode_proposal.sh — build the osmosis-1 governance proposal
#                              that stores the three Bluechip wasms
# =====================================================================
# usage: scripts/make_storecode_proposal.sh [deposit_uosmo]
#
#   deposit_uosmo   Initial deposit attached at submission, in uosmo.
#                   Defaults to 6_000_000_000 (6,000 OSMO), the osmosis-1
#                   v1 min_deposit as of 2026-07. Re-check before
#                   submitting:
#                     osmosisd query gov params --node <mainnet> -o json \
#                       | jq '.params.min_deposit'
#                   A smaller deposit is legal — the proposal then sits
#                   in the deposit period until the community tops it up.
#
# Produces gov/storecode_proposal.json: ONE gov v1 proposal carrying
# three /cosmwasm.wasm.v1.MsgStoreCode messages (factory, creator_pool,
# router), each gzip-compressed and executed by the governance module
# account. One community vote stores all three code IDs.
#
# Design choices baked in:
#   - sender on each MsgStoreCode is the gov module account — REQUIRED
#     for a governance-executed store; any other sender fails at
#     execution with an authority error.
#   - instantiate_permission is Everybody on all three. The factory must
#     instantiate the creator-pool code at runtime, and the factory's
#     own address does not exist until after the proposal passes, so it
#     cannot be allowlisted here. Stray third-party instantiations are
#     inert: the factory's registry only trusts pools it created itself
#     through its reply chain.
#   - wasm_byte_code is gzip-9 compressed (wasmd auto-detects and
#     decompresses at execution) to keep the proposal JSON ~4x smaller.
#
# Reproducibility guard: refuses to run on a dirty git tree, and stamps
# the commit hash + artifact sha256s into the proposal summary so voters
# can rebuild the exact bytes with cosmwasm/optimizer 0.16.0.
#
# Prerequisites: artifacts/ built via `make optimize-all` from a clean,
# pushed commit. NEVER include factory-integration.wasm (short-timelock
# test build) — this script only touches the three prod artifacts.
#
# After the proposal passes:
#   1. Read the three code IDs from the proposal execution events (or
#      `osmosisd query wasm list-code`).
#   2. Fill CREATOR_POOL_CODE_ID / FACTORY_CODE_ID / ROUTER_CODE_ID (and
#      the reused CW20/CW721 IDs) in osmosis_mainnet.env.
#   3. ./deploy_osmosis.sh osmosis_mainnet.env   (STORE_MODE=gov skips
#      uploads and only instantiates factory + router).
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="$REPO_ROOT/artifacts"
OUT_DIR="$REPO_ROOT/gov"
OUT_FILE="$OUT_DIR/storecode_proposal.json"

# Governance module account on osmosis-1. Verify anytime with:
#   osmosisd query auth module-account gov --node <mainnet> -o json
GOV_AUTHORITY="osmo10d07y265gmmuvt4z0w9aw880jnsr700jjeq4qp"

DEPOSIT_UOSMO="${1:-6000000000}"

for cmd in jq gzip base64 sha256sum git; do
    command -v "$cmd" >/dev/null || { echo "error: $cmd not on PATH" >&2; exit 1; }
done

# ---- Reproducibility guard -----------------------------------------
# Tracked modifications only: untracked files (e.g. local state files,
# generated gov/ output) cannot change what a voter rebuilds from the
# commit, but any modified tracked source would make the embedded
# hashes unreproducible.
if [ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=no)" ]; then
    echo "error: tracked files are modified — voters must be able to rebuild" >&2
    echo "       the artifact hashes from a public commit. Commit (and push)" >&2
    echo "       everything, re-run \`make optimize-all\`, then re-run this." >&2
    exit 1
fi
COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"

WASMS=(factory.wasm creator_pool.wasm router.wasm)
LABELS=("Factory (registry + admin control plane, 48h timelocks)" \
        "Creator pool (two-phase commit/AMM pool)" \
        "Router (multi-hop swaps)")

for w in "${WASMS[@]}"; do
    [ -f "$ARTIFACTS/$w" ] || {
        echo "error: missing $ARTIFACTS/$w — run \`make optimize-all\` first" >&2
        exit 1
    }
done

mkdir -p "$OUT_DIR"

# ---- Hash manifest (goes into the summary voters read) --------------
HASH_LINES=""
for i in "${!WASMS[@]}"; do
    h="$(sha256sum "$ARTIFACTS/${WASMS[$i]}" | cut -d' ' -f1)"
    HASH_LINES="${HASH_LINES}- ${WASMS[$i]} (${LABELS[$i]}): sha256 ${h}
"
done

TITLE="Store Bluechip protocol CosmWasm contracts (factory, creator-pool, router)"
SUMMARY="This proposal stores the three CosmWasm contracts of the Bluechip creator-token launchpad on Osmosis.

Bluechip lets a creator launch a CW20 token paired against OSMO with no upfront liquidity: supporters commit OSMO, and when a pool's cumulative committed value crosses its USD threshold (valued via the chain's own x/twap over the main OSMO/USDC pool — no external oracle, no keeper) the pool mints the fixed token supply, self-seeds a constant-product AMM from the committed OSMO, and distributes tokens to committers pro-rata. Post-threshold the pool is a standard xyk market with NFT-tracked LP positions. Every admin mutation (config, per-pool config, pool code upgrades) sits behind a 48-hour timelock enforced on-chain by the factory.

Artifacts (reproducible via cosmwasm/optimizer 0.16.0 from commit ${COMMIT}):
${HASH_LINES}
Instantiate permission is Everybody on all three code IDs: the factory must instantiate the creator-pool code at runtime, and the factory's address cannot be allowlisted before it exists. Third-party instantiations are inert — the factory's registry only recognizes pools it created itself.

If this proposal passes, the team instantiates the factory and router (creator tokens use an existing audited cw20-base code ID; LP position NFTs use cw721-base). Pools only ever hold OSMO plus their own project-minted CW20; there is no privileged mint of OSMO and no bridged-asset exposure.

Full rehearsal of the protocol lifecycle (including threshold crossing, multi-wallet pro-rata distribution, timelocked pool migration, and the two-phase emergency-withdraw flow) was completed on osmo-test-5; see the linked forum post for transaction evidence.

Forum discussion: <FORUM-LINK-HERE>
Repository: <REPO-URL-HERE> (commit ${COMMIT})"

# ---- Build the three MsgStoreCode messages --------------------------
store_msg() {
    # The base64 payload exceeds Linux's per-argument limit (~128KiB),
    # so it must reach jq via --rawfile, not --arg.
    local file="$1" b64tmp
    b64tmp="$(mktemp)"
    gzip -9 -c "$ARTIFACTS/$file" | base64 -w0 > "$b64tmp"
    jq -n \
        --arg sender "$GOV_AUTHORITY" \
        --rawfile code "$b64tmp" \
        '{
            "@type": "/cosmwasm.wasm.v1.MsgStoreCode",
            sender: $sender,
            wasm_byte_code: ($code | rtrimstr("\n")),
            instantiate_permission: {
                permission: "ACCESS_TYPE_EVERYBODY",
                addresses: []
            }
        }'
    rm -f "$b64tmp"
}

# Combined messages array also exceeds the per-argument limit — route
# it through a temp file with --slurpfile.
MSGS_TMP="$(mktemp)"
for w in "${WASMS[@]}"; do store_msg "$w"; done | jq -s . > "$MSGS_TMP"

jq -n \
    --slurpfile messages "$MSGS_TMP" \
    --arg title "$TITLE" \
    --arg summary "$SUMMARY" \
    --arg deposit "${DEPOSIT_UOSMO}uosmo" \
    '{
        messages: $messages[0],
        metadata: "<FORUM-LINK-HERE>",
        deposit: $deposit,
        title: $title,
        summary: $summary,
        expedited: false
    }' > "$OUT_FILE"
rm -f "$MSGS_TMP"

jq empty "$OUT_FILE"   # hard-fail if the JSON is malformed

echo "wrote $OUT_FILE ($(du -h "$OUT_FILE" | cut -f1)) from commit $COMMIT"
echo ""
echo "artifact hashes embedded in the summary:"
printf '%s' "$HASH_LINES"
echo ""
echo "BEFORE SUBMITTING:"
echo "  - replace <FORUM-LINK-HERE> (metadata + summary) and <REPO-URL-HERE>"
echo "  - re-check the live min deposit:"
echo "      osmosisd query gov params --node https://rpc.osmosis.zone:443 -o json | jq '.params.min_deposit'"
echo ""
echo "submit with:"
echo "  osmosisd tx gov submit-proposal $OUT_FILE \\"
echo "    --from deployer --chain-id osmosis-1 \\"
echo "    --node https://rpc.osmosis.zone:443 \\"
echo "    --gas auto --gas-adjustment 1.4 --gas-prices 0.025uosmo"
