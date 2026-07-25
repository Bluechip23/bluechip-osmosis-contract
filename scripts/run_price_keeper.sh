#!/usr/bin/env bash
# =====================================================================
# run_price_keeper.sh — launch the Pyth price keeper from a deploy env
# =====================================================================
# usage:
#   ENV_FILE=osmo_testnet.env scripts/run_price_keeper.sh
#   # or just: scripts/run_price_keeper.sh          (defaults to osmo_testnet.env)
#
# The factory values commits from the Pyth OSMO/USD feed and fails closed
# if the on-chain price is stale. This launches the keeper that keeps that
# feed fresh (fetch from Hermes -> update_price_feeds). It sources the
# SAME env file the deploy uses and maps its shell-style vars onto the
# CosmJS keeper's env var names, so testnet vs mainnet is one file.
#
# The keeper signs with a MNEMONIC (CosmJS cannot read the osmosisd keyring
# that FROM= uses), and that mnemonic is a SECRET, so it is NOT read from
# the (committed) deploy env. Provide it one of two ways:
#   - put `KEEPER_MNEMONIC="..."` in keepers/.env  (gitignored), or
#   - export KEEPER_MNEMONIC in your shell before running this.
# Give the keeper its OWN wallet in production; on testnet the throwaway
# alice mnemonic is fine for a rehearsal.
# =====================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ENV_FILE:-osmo_testnet.env}"
ENV_PATH="$REPO_ROOT/$ENV_FILE"

if [ ! -f "$ENV_PATH" ]; then
  echo "error: env file not found: $ENV_PATH" >&2
  exit 1
fi

# 1. Secret (mnemonic) from the gitignored keeper env, if present.
if [ -f "$REPO_ROOT/keepers/.env" ]; then
  # shellcheck disable=SC1091
  set -a; source "$REPO_ROOT/keepers/.env"; set +a
fi

# 2. Chain + Pyth config from the deploy env (this WINS over keepers/.env
#    for the mapped fields below).
# shellcheck disable=SC1090
set -a; source "$ENV_PATH"; set +a

# 3. Map deploy-env (shell/osmosisd) names -> keeper (CosmJS) names.
export RPC_ENDPOINT="${NODE:?NODE must be set in $ENV_FILE}"
export CHAIN_ID="${CHAIN_ID:?CHAIN_ID must be set in $ENV_FILE}"
export BECH32_PREFIX="${BECH32_PREFIX:-osmo}"
export GAS_PRICE="${GAS_PRICES:?GAS_PRICES must be set in $ENV_FILE}"
export GAS_DENOM="${NATIVE_DENOM:-uosmo}"
export PYTH_CONTRACT_ADDR="${PYTH_CONTRACT_ADDR:?PYTH_CONTRACT_ADDR must be set in $ENV_FILE}"
export PYTH_NATIVE_USD_FEED_ID="${PYTH_NATIVE_USD_FEED_ID:?PYTH_NATIVE_USD_FEED_ID must be set in $ENV_FILE}"
export HERMES_ENDPOINT="${HERMES_ENDPOINT:-https://hermes-beta.pyth.network}"
export PYTH_PUSH_INTERVAL_MS="${PYTH_PUSH_INTERVAL_MS:-60000}"
export MIN_KEEPER_BALANCE="${MIN_GAS_BALANCE:-1000000}"

if [ -z "${KEEPER_MNEMONIC:-}" ]; then
  echo "error: KEEPER_MNEMONIC is not set." >&2
  echo "       Put KEEPER_MNEMONIC=\"...\" in keepers/.env (gitignored)," >&2
  echo "       or export it in your shell before running this script." >&2
  exit 1
fi
export KEEPER_MNEMONIC

echo "launching price keeper:"
echo "  chain   = $CHAIN_ID  ($RPC_ENDPOINT)"
echo "  pyth    = $PYTH_CONTRACT_ADDR"
echo "  feed    = $PYTH_NATIVE_USD_FEED_ID"
echo "  hermes  = $HERMES_ENDPOINT"
echo "  cadence = ${PYTH_PUSH_INTERVAL_MS}ms"
echo ""

# Ensure keeper deps are present.
if [ ! -d "$REPO_ROOT/keepers/node_modules" ]; then
  echo "installing keeper deps (npm install)..." >&2
  npm --prefix "$REPO_ROOT/keepers" install --no-audit --no-fund >&2
fi

exec npm --prefix "$REPO_ROOT/keepers" run price-keeper
