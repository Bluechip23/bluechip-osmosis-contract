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
  `cw721_base.wasm` at the repo root only satisfy legacy factory
  config fields (unused at runtime post-TokenFactory migration);
  prefer pinning already-stored code IDs via `CW20_CODE_ID` /
  `CW721_CODE_ID` in the env.

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
| `cross_threshold.sh <pool> [amount]` | commit OSMO past the native threshold (auto-sized from the remaining gap). Crossing queues the airdrop; it does not mint or create the native pool |
| `continue_distribution.sh <pool>` | flush the committer payout batches, then make the follow-up call that mints the remaining splits and creates the native pool (trading opens here) |
| `swap.sh <pool> native\|token <amt>` | AMM swap either direction (simulates first); only after the distribution has finished and the pool is open |
| `liquidity.sh deposit\|shares\|remove` | LP on the NATIVE GAMM pool (MsgJoinPool/MsgExitPool; discovers the pool id via the contract's `native_pool_id` query) |
| `route_swap.sh buy\|hop ...` | router swaps: single hop OSMO→token, or 2-hop tokenA→OSMO→tokenB |
| `status.sh [pool]` | factory/router/pool health (config, fee-route probe, distribution state, pending factory-notify) |
| `run_lifecycle_test.sh` | end-to-end: create → commit → cross → distribute (pool opens) → swap → LP → route, with a pass/fail summary |
| `_helpers.sh` | shared tx/query plumbing (sourced, not run) |

## Testnet tips

- Drop `COMMIT_THRESHOLD_LIMIT_NATIVE` in `osmo_testnet.env` to a few
  hundred OSMO **before deploying** so a threshold cross is cheap (it
  must stay above the contract's 115-OSMO pre-threshold minimum commit).
- `PRICING_POOL_ID` must point at a real OSMO/stable pool with at least
  10 minutes of TWAP history; the deploy's fee-route probe tells you
  immediately if the route is broken.
- Pool creation is limited to 1/hour/address on the prod factory
  build. For rapid-fire testing deploy with
  `FACTORY_WASM_FILE=factory-integration.wasm ./deploy_osmosis.sh`
  (short timelocks/limits — **never** ship that wasm to mainnet).
- Direct commits and swaps share a 13s per-wallet cooldown (router
  swaps and native Osmosis trades are not subject to it); the scripts
  sleep around it where it matters.
- Swaps, post-threshold commits and LP only work once
  `continue_distribution.sh` has finished: until then the native pool
  does not exist and the contract answers `DistributionInProgress`.
