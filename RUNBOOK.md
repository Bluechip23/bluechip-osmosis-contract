# BlueChip Production Runbook (Osmosis)

How to operate this stack in production. The old runbook's oracle
pipeline — Pyth pusher, oracle keeper, anchor heartbeat, bounty
funding — is **gone**: USD pricing is now a single stateless query
against Osmosis's own `x/twap` module, so there is nothing to keep
alive for prices. What remains to operate is exactly one recurring
job (the distribution keeper), one standing monitor (the pricing
canary), and governance hygiene around the 48h timelocks.

Money never moves incorrectly when your infra dies; it just stops
moving until someone calls the permissionless entry points again.
That fail-closed property is deliberate — operate to it.

All constants below are quoted from source; paths given so they can
be re-verified after any contract change.

## The timing constants that matter

| Constant | Value | Where | Meaning |
|---|---|---|---|
| `TWAP_WINDOW_MIN/MAX_SECONDS` | **300 / 3600 s** (deployed: 600) | `factory/src/usd_price.rs` | Lookback of the x/twap price behind every commit valuation. The manipulation cost of the USD threshold is the cost of moving the pricing pool for this long |
| `RATE_MAX` | **$10,000/native** | `factory/src/usd_price.rs` | Sanity ceiling on the parsed rate; a rate above it (wrong-decimals quote denom, spiked pool) makes commits revert rather than misprice |
| Distribution stall timeout | 24 h | `creator-pool` (`DISTRIBUTION_STALL_TIMEOUT_SECONDS`) | After this, batches reject and admin recovery is required |
| Public distribution recovery | 7 days | `creator-pool` (`SelfRecoverDistribution`) | Anyone may restart a stalled distribution after this |
| Admin config changes | 48 h | factory / router timelocks | Every propose→apply pair needs calendared two-step execution |
| Emergency-withdraw delay | config (`EMERGENCY_WITHDRAW_DELAY_SECONDS`, mainnet 86400) | factory config | Gap between EW initiate and drain on every pool |

## The one recurring job: the distribution keeper

A threshold cross pays **nobody** in the crossing transaction —
recipients are flushed in gas-budgeted batches (≤40/tx) by
`ContinueDistribution` calls until the ledger drains. The call is
permissionless but carries **no bounty anymore**, so no third party
will make it for you: the protocol runs the keeper, and its only cost
is gas.

One process covers everything (`keepers/`, `npm run
distribution-keeper` — see `keepers/.env.example` for the Osmosis
config):

- **`continue_distribution`** sweeps every commit pool (auto-discovered
  from the factory registry — leave `POOL_ADDRESSES` unset) and drains
  any in-flight distribution. Alert on
  `distribution_state.is_stalled` (24 h timeout →
  `RecoverPoolStuckStates` from the factory admin; after 7 days anyone
  can `self_recover_distribution`).
- **`retry_factory_notify`** pre-pass: the factory notification at
  threshold cross is deliberately deferred-on-error; the keeper
  retries any pool reporting `factory_notify_status.pending == true`
  (idempotent — the factory's crossing gate makes double-processing
  impossible). Retries that keep failing are an ops page.
- **`PruneRateLimits`** housekeeping, folded in once a day by default.

## The passive contracts

**Router** — no bot needed. Put its admin behind the multisig
(`docs/MULTISIG.md`) and monitor for unexpected
`propose_config_update` events (the 48 h timelock is your reaction
window). Simulation resolves each hop against the factory registry,
quotes come from `POOL_STATE` accounting reserves, and each hop's
`max_spread` is pinned to the pools' 5% hard cap so
`minimum_receive` is the binding slippage control.

**Factory** — no bot needed. Its one live dependency is the pricing
route (below).

## Infrastructure rules

- **Supervision, not terminals.** The keeper runs under systemd
  (`Restart=always`) or a k8s deployment. Nothing fails closed if it
  lags — distributions just wait — but committers are watching.
- **One dedicated key per bot**, never shared with admin/treasury —
  two processes signing with one key produce account-sequence races.
  Keep balances low, top up from treasury, alert below threshold
  (`MIN_KEEPER_BALANCE_UBLUECHIP`, denominated in `GAS_DENOM`).
- **RPC redundancy**: primary + fallback endpoints; ideally run your
  own node.
- **Post-deploy verification**: `./deploy_osmosis.sh <env>` ends with
  a factory-config readback and a live `ConvertNativeToUsd` probe;
  re-run those two queries manually after every timelocked config
  change lands.

## Monitoring

**The single best health probe:** query

```json
{"pool_factory_query":{"convert_native_to_usd":{"amount":"1000000"}}}
```

on the **factory** every minute; page if it errors. Commit valuation
is fail-closed through this exact path, so a green probe proves the
entire pricing route (pool id, denom pair, TWAP window, rate sanity
gates) that every commit depends on. There is no staleness dimension —
the TWAP is computed at query time by the chain.

**The one risk the probe can't see: pricing-pool liquidity decay.**
x/twap never reports "stale" — a draining pool keeps returning prices
while the cost of manipulating them silently falls. Alarm on the
liquidity of `pricing_pool_id` (via Osmosis LCD/indexer) dropping
below a floor you choose; if OSMO/USDC liquidity migrates to a newer
pool over time, move `pricing_pool_id` with it via the 48 h config
flow (the propose/apply both live-probe the new route before it can
land).

Secondary alerts:

- keeper wallet gas balance
- any pool with `factory_notify_status.pending == true` for > 1 h
- any pool with `distribution_state.is_stalled == true`
- unexpected `propose_config_update` events on factory / router
  (the 48 h timelock is your reaction window)
- rate returned by the canary drifting far from a reference OSMO/USD
  price (exchange API) — catches pricing-pool manipulation attempts

## Reference topology

One distribution keeper under supervision; the pricing canary + the
liquidity-floor alarm in your monitoring stack; a dashboard. That's
the whole footprint — one small process and two alerts.

## Governance hygiene

Factory admin, router admin, contract (migration) admin, and
`PROTOCOL_WALLET` → multisig (`docs/MULTISIG.md` has the full setup
and signing walkthrough). Every admin action is 48 h propose→apply:
calendar both steps, monitor the pending-proposal state between them,
and treat an unexpected pending proposal as an incident.
