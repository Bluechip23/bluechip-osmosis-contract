# Osmosis testnet deploy + test scripts

Shell tooling for deploying the stack (factory + creator-pool +
router) to **osmo-test-5** and exercising the full protocol lifecycle
against it. Every script reads `osmo_testnet.env` (override with
`ENV_FILE=<path>`) plus the state file the deploy writes
(`osmo_testnet.state`); per-script usage lives in each header.

## Prerequisites

- `osmosisd` and `jq` on PATH
- a funded key in the local keyring matching `FROM` in the env file
  (faucet: <https://faucet.testnet.osmosis.zone/>)
- wasms built into `artifacts/` — `make optimize-all` (reproducible)
  or `make build` (fast, testnet only). `cw20_base.wasm` /
  `cw721_base.wasm` are picked up from the repo root automatically.

## Quick start

```bash
make build                                   # or: make optimize-all
./deploy_osmosis.sh osmo_testnet.env         # store wasms, instantiate, verify
scripts/status.sh                            # health overview
scripts/run_lifecycle_test.sh                # full automated rehearsal
```

## Script map

| Script | What it does |
|---|---|
| `../deploy_osmosis.sh <env>` | store the 5 wasms, instantiate factory + router, verify (config readback + live x/twap probe). Resumable via the state file; handles mainnet gov-mode code IDs too |
| `create_commit_pool.sh <name> <symbol>` | factory.Create; logs the new pool to `commit_pools.txt` |
| `cross_threshold.sh <pool> [amount]` | commit OSMO past the USD threshold (auto-sized at the live x/twap rate) |
| `continue_distribution.sh <pool>` | flush the post-threshold committer payout batches |
| `swap.sh <pool> native\|token <amt>` | AMM swap either direction (simulates first) |
| `liquidity.sh deposit\|positions\|collect\|remove` | LP lifecycle incl. the CW20 allowance dance |
| `route_swap.sh buy\|hop ...` | router swaps: single hop OSMO→token, or 2-hop tokenA→OSMO→tokenB |
| `status.sh [pool]` | factory/router/pool health — the same signals the RUNBOOK alerts on |
| `run_lifecycle_test.sh` | end-to-end: create → commit → cross → distribute → swap → LP → route, with a pass/fail summary |
| `_helpers.sh` | shared tx/query plumbing (sourced, not run) |

## Testnet tips

- Drop `COMMIT_THRESHOLD_LIMIT_USD` in `osmo_testnet.env` to a few
  hundred dollars **before deploying** so a threshold cross is cheap.
- `PRICING_POOL_ID` must point at a real OSMO/USD-stable pool with
  enough TWAP history for `TWAP_WINDOW_SECONDS`; the deploy's pricing
  probe tells you immediately if the route is broken.
- Pool creation is limited to 1/hour/address on the prod factory
  build. For rapid-fire testing deploy with
  `FACTORY_WASM_FILE=factory-integration.wasm ./deploy_osmosis.sh`
  (short timelocks/limits — **never** ship that wasm to mainnet).
- Commits and swaps share a 13s per-wallet rate limit; the scripts
  sleep around it where it matters.
