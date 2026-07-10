# Bluechip Keepers

> Keeper actions pay **no bounty** — the operator absorbs gas costs as
> part of running the protocol. There is no price-oracle keeper: USD
> pricing is chain-native (Osmosis `x/twap`) and needs no off-chain
> upkeep.

One off-chain bot keeps the Bluechip protocol tidy — the **distribution
keeper**, which per sweep:

- calls each pool's `ContinueDistribution` when it has an active
  post-threshold distribution, so committers receive their creator tokens in
  a reasonable timeframe;
- retries stuck `NotifyThresholdCrossed` submessages via each pool's
  permissionless `RetryFactoryNotify` (query-first, so it only spends gas on
  pools that actually report `pending=true`);
- periodically dispatches the factory's permissionless `PruneRateLimits`
  storage-hygiene sweep.

All three actions are permissionless and bounty-less.

## Prerequisites

- Node 20+
- A funded keeper wallet (gas only — it will not self-replenish)
- The factory contract address

## One-time setup

```sh
cd keepers
npm install
cp .env.example .env
# edit .env — fill in RPC_ENDPOINT, FACTORY_ADDRESS, KEEPER_MNEMONIC, etc.
npm test          # run the unit tests to confirm the build is sane
npm run typecheck # confirm nothing's broken at the type level
```

## Running

```sh
npm run distribution-keeper    # never exits; runs until SIGTERM
```

In production, run it under `systemd` (or Docker, or Cloud Run). The
process is stateless — crash recovery is just "restart it." Do not run
two instances with the **same** mnemonic (they'll fight on sequence
numbers).

### systemd example

```ini
# /etc/systemd/system/bluechip-distribution-keeper.service
[Unit]
Description=Bluechip distribution keeper
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/bluechip-keepers
Environment=NODE_ENV=production
EnvironmentFile=/opt/bluechip-keepers/.env
ExecStart=/usr/bin/npm run distribution-keeper
Restart=always
RestartSec=10
User=bluechip

[Install]
WantedBy=multi-user.target
```

## Funding the keeper wallet

There are no bounties, so the wallet is pure gas spend. Size it for your
expected sweep cadence and top up when the keeper logs
`keeper balance below threshold`. Distribution batches are the only
recurring cost; retry-notify and prune txs fire rarely.

## How the keeper decides when to act

```
every DISTRIBUTION_POLL_INTERVAL_MS (default 30 min):
  # Resolve watch list: explicit POOL_ADDRESSES if set, otherwise
  # auto-discover every commit pool from the factory registry.

  # Pre-sweep: settle any stuck factory-notify state — see "Retry-notify".
  for each pool in watch list:
    query pool.FactoryNotifyStatus {}
    if pending=true → submit pool.RetryFactoryNotify {}

  # Main sweep: process pending committer payouts.
  for each pool in watch list:
    loop (bounded):
      submit pool.ContinueDistribution {}
      if NothingToRecover → move to next pool
      if ok + distribution_complete=false → continue same pool
      otherwise → move to next pool

  warn if wallet balance < MIN_KEEPER_BALANCE_UBLUECHIP

  # Folded-in maintenance — see "Rate-limit prune".
  every PRUNE_EVERY_N_SWEEPS sweeps (default 48 ≈ once a day):
    submit factory.PruneRateLimits { batch_size: PRUNE_BATCH_SIZE }

  if any pool made progress this sweep, re-sweep in ~15s instead of 30 min
```

The per-pool inner loop is capped at 200 batches per sweep as a safety valve.
Exceeding that is logged loudly — it means something is stuck.

### Retry-notify

When a commit pool crosses its threshold, it dispatches
`NotifyThresholdCrossed` to the factory as a `reply_on_error` SubMsg. The
factory records the crossing in its `POOL_THRESHOLD_CROSSED` registry. If
the factory rejects (a transient issue), `PENDING_FACTORY_NOTIFY` flips to
`true` on the pool until somebody calls `RetryFactoryNotify`.

The contract handler is permissionless on purpose — anyone can settle the
stuck notify. The keeper polls each pool's `FactoryNotifyStatus` query
first and only spends gas on the (rare) pools that report `pending=true`.
The factory's `POOL_THRESHOLD_CROSSED` idempotency gate ("Threshold
crossing already recorded for this pool") makes a redundant retry
harmless: at worst the keeper wastes its own gas.

### Rate-limit prune

The factory tracks per-address create cooldowns in
`LAST_COMMIT_POOL_CREATE_AT`. This map grows monotonically — every new
creator address adds an entry that is never removed by the cooldown
logic itself. `PruneRateLimits` is a
permissionless handler that removes entries older than 10× the cooldown
(currently 10 hours). The keeper dispatches it once every
`PRUNE_EVERY_N_SWEEPS` sweeps rather than running a separate bot because
the cadence is wildly relaxed (daily is plenty) and the keeper already
runs a periodic loop.

Set `PRUNE_EVERY_N_SWEEPS=0` to disable the sweep entirely (e.g., on a
testnet where storage growth doesn't matter, or if you'd rather run
prune from a separate cron).

## Monitoring

Every log line is structured JSON. Pipe into your aggregator of choice (Loki,
Datadog, CloudWatch). The events you want to alert on:

| Level | Message | Action |
|-------|---------|--------|
| `error` | `distribution keeper crashed` | Page. Restart the process. |
| `warn` | `keeper balance below threshold` | Top up the wallet. |
| `error` | `distribution batch tx failed` / `distribution call errored` | Investigate — a pool's distribution is stuck for an unexpected reason. |
| `error` | `retry_factory_notify errored` | Investigate — pool reported pending notify but retry failed for an unexpected reason. |
| `warn` | `factory_notify_status query failed` | Single-pool RPC blip; ignore unless persistent. |
| `warn` | `rate-limit prune errored (non-fatal)` | Investigate — prune is best-effort; persistent failures should be looked at. |

And a liveness check: if you haven't seen a `distribution keeper starting`
or `sleeping` log line in >45 minutes, assume it's hung and restart it.

## Testing

```sh
npm test          # unit tests — decision logic + config + loop behavior
npm run typecheck # type safety
```

The unit tests cover every pure function driving the loops plus
integration-style runs against an in-memory contract mock. They do not
require a running chain. Before deploying to mainnet, also smoke-test
against a testnet:

1. Deploy factory + pool to a Cosmos testnet.
2. Point `.env` at the testnet endpoint.
3. Run the keeper for at least a week.
4. Verify the log stream shows the expected mix of ok/no-op outcomes.

## Failure modes you should know about

- **Keeper wallet runs out of gas.** Txs stop going through. You'll see
  `keeper balance below threshold` warnings before this happens if you
  kept `MIN_KEEPER_BALANCE_UBLUECHIP` set to something sane. There is no
  bounty income, so this WILL eventually happen without topping up.

- **Keeper is down for a while.** Nothing breaks: distributions simply
  pause mid-flight (committers wait longer for their creator tokens),
  stuck notifies stay pending, and rate-limit maps grow slightly. All
  three actions are permissionless, so anyone can advance them manually
  in the meantime.

## Layout

```
src/
├── lib/
│   ├── config.ts             # env parsing (zod-validated)
│   ├── client.ts             # CosmJS wallet + signing client
│   ├── decisions.ts          # pure tx-outcome classification
│   ├── balance.ts            # keeper gas-runway warning
│   ├── discovery.ts          # commit-pool auto-discovery via factory registry
│   ├── logger.ts             # structured JSON output
│   ├── types.ts              # contract message + query shapes
│   ├── distribution-loop.ts  # per-pool ContinueDistribution drain
│   ├── prune-loop.ts         # one PruneRateLimits iteration
│   └── retry-notify-loop.ts  # query-then-retry per pool
├── __tests__/
│   ├── config.test.ts
│   ├── decisions.test.ts
│   ├── discovery.test.ts
│   ├── distribution-keeper.integration.test.ts
│   ├── prune-loop.test.ts
│   └── retry-notify-loop.test.ts
└── distribution-keeper.ts    # entrypoint: retry-notify + distribution + prune
```
