# BlueChip Production Runbook (Osmosis)

How to operate this stack in production. USD pricing comes from the
**Pyth** OSMO/USD feed (NOT an on-chain pool TWAP — the OSMO/USD pool
substrate is too thin to price safely; see
`MAINNET_LIQUIDITY_RECON.md` / `ORACLE_PYTH_TRANSITION.md`). Because
Pyth on Osmosis is push-based and nobody keeps OSMO/USD fresh on-chain,
the operational surface is now **two** recurring jobs — the distribution
keeper AND the price keeper — plus one standing monitor (the pricing
canary) and governance hygiene around the 48h timelocks.

> **⚠ CHANGED from the x/twap design:** there IS now something to keep
> alive for prices. If the price keeper stops, the factory's staleness
> gate starts failing every commit closed once the last pushed price
> ages past `max_pyth_staleness_seconds`. Fail-safe (no mispricing, no
> fund loss) — but commits halt until the keeper resumes.

Money never moves incorrectly when your infra dies; it just stops
moving until the keepers resume and someone calls the permissionless
entry points again. That fail-closed property is deliberate — operate
to it.

All constants below are quoted from source; paths given so they can
be re-verified after any contract change.

## The timing constants that matter

| Constant | Value | Where | Meaning |
|---|---|---|---|
| `max_pyth_staleness_seconds` | **300 s** default (range 30–600) | `factory` config / `usd_price.rs` | A Pyth price older than this fails closed. Set it above the price keeper's push cadence + slack (e.g. 60s push ⇒ 300s gate = 4 missed-push tolerance) |
| `MIN_PYTH_AGE_SECONDS` | **10 s** | `factory/src/usd_price.rs` | A price must be at least this old to be consumed — forces the keeper's push and the consuming commit into DIFFERENT blocks (anti same-block-MEV) |
| `pyth_conf_threshold_bps` | **200 bps** default (range 50–500) | `factory` config / `usd_price.rs` | Reject a Pyth price whose confidence interval exceeds this fraction (feed too dispersed) |
| `RATE_MAX` | **$10,000/native** | `factory/src/usd_price.rs` | Sanity ceiling on the parsed rate; a rate above it makes commits revert rather than misprice |
| Distribution stall timeout | 24 h | `creator-pool` (`DISTRIBUTION_STALL_TIMEOUT_SECONDS`) | After this, batches reject and admin recovery is required |
| Public distribution recovery | 7 days | `creator-pool` (`SelfRecoverDistribution`) | Anyone may restart a stalled distribution after this |
| Admin config changes | 48 h | factory / router timelocks | Every propose→apply pair needs calendared two-step execution |
| Emergency-withdraw delay | config (`EMERGENCY_WITHDRAW_DELAY_SECONDS`, mainnet 86400) | factory config | Gap between EW initiate and drain on every pool |

## The price keeper (npm run price-keeper) — REQUIRED

The factory reads the OSMO/USD price from the Pyth CW contract and fails
closed past `max_pyth_staleness_seconds`. Pyth on Osmosis stores only the
last price *someone* pushed, and nobody keeps OSMO/USD fresh (when
measured, every feed on the mainnet Pyth contract was ~115 days stale).
So this keeper must run continuously:

1. fetch the latest signed OSMO/USD update from Pyth **Hermes**,
2. query the Pyth contract's update fee (validated live: **1 uosmo**),
3. submit `update_price_feeds { data: [...] }` with the fee attached.

Config: `keepers/.env.example` (`PYTH_CONTRACT_ADDR`,
`PYTH_NATIVE_USD_FEED_ID`, `HERMES_ENDPOINT`, `PYTH_PUSH_INTERVAL_MS`).
Give it its **own wallet** (never shared with the distribution keeper or
admin — two processes on one key race on account sequence numbers).
Supervise it (systemd `Restart=always` / k8s). **Alert if the last
successful push is older than the staleness gate minus one interval** —
that is your last warning before commits start failing closed.

Testnet note: mainnet Hermes serves mainnet-guardian-signed prices for
mainnet Pyth contracts. A testnet Pyth contract verifies against a
different (beta) guardian set, so point `HERMES_ENDPOINT` at Pyth's beta
Hermes when pushing to osmo-test-5.

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

**Factory** — its one live dependency is the price keeper (above): the
Pyth OSMO/USD feed must stay fresh or commits fail closed. No other bot.

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
entire pricing route (Pyth contract, feed id, staleness + confidence +
sanity gates) that every commit depends on. Unlike the old x/twap
design, this probe DOES have a staleness dimension: if the price keeper
lags, the probe goes red before commits do — it is your earliest
warning.

**The load-bearing dependency the probe reflects: the price keeper.**
A red canary almost always means the keeper stopped pushing and the
last Pyth price aged past `max_pyth_staleness_seconds`. Alarm on: the
keeper's last-successful-push age exceeding the staleness gate minus one
push interval (earliest signal); the keeper wallet gas balance; and the
canary rate drifting far from a reference OSMO/USD price (exchange API)
— which catches a wrong feed id or a bad push. The `pricing_pool_id`
still exists but only as the fee-swap route at crossing, so it needs
only enough depth to fill the ~$20 USDC creation fee, not
manipulation-resistant depth.

Secondary alerts:

- price-keeper wallet gas balance + last-push age
- distribution-keeper wallet gas balance
- any pool with `factory_notify_status.pending == true` for > 1 h
- any pool with `distribution_state.is_stalled == true`
- unexpected `propose_config_update` events on factory / router
  (the 48 h timelock is your reaction window)
- rate returned by the canary drifting far from a reference OSMO/USD
  price (exchange API) — catches pricing-pool manipulation attempts

## Reference topology

**Two** keepers under supervision, each with its own wallet: the price
keeper (`npm run price-keeper`, keeps Pyth OSMO/USD fresh — commits
depend on it) and the distribution keeper (`npm run distribution-keeper`).
Plus the pricing canary + keeper-last-push/gas alarms in your monitoring
stack, and a dashboard. Two small processes; the price keeper is the one
whose outage stops commits.

## Governance hygiene

Factory admin, router admin, contract (migration) admin, and
`PROTOCOL_WALLET` → multisig (`docs/MULTISIG.md` has the full setup
and signing walkthrough). Every admin action is 48 h propose→apply:
calendar both steps, monitor the pending-proposal state between them,
and treat an unexpected pending proposal as an incident.
