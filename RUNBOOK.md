# BlueChip Production Runbook

How to operate this stack in production. The key mental split: the
**router** and **expand-economy** are *passive* contracts — they need
governance and funding, not babysitting. The **oracle pipeline** is
*active* — three recurring jobs that, if any one stops, make the whole
protocol **fail closed**: commits and pool creation start reverting
within minutes. Money never moves incorrectly when your infra dies; it
just stops moving until someone calls the permissionless entry points
again. That property is deliberate (bounties, permissionless recovery,
fail-closed gates) — operate to it.

All constants below are quoted from source; paths given so they can be
re-verified after any contract change.

## The timing constants that matter

| Constant | Value | Where | Meaning |
|---|---|---|---|
| `UPDATE_INTERVAL` | **60 s** | `factory/src/internal_bluechip_price_oracle.rs` | Minimum gap between oracle publishing rounds — `update_oracle_price` is permissionless but rate-limited to this |
| `MAX_ORACLE_STALENESS_SECONDS` | **120 s** | `creator-pool/src/swap_helper.rs` | Pool-side gate: commit valuation rejects once the published price is older than this. **This is the gate users hit first.** |
| `MAX_PRICE_AGE_SECONDS_BEFORE_STALE` | **300 s** | `factory/src/state.rs` | Factory-side gate on the *Pyth data* the oracle consumes |
| Distribution stall timeout | 24 h | `creator-pool` (`DISTRIBUTION_STALL_TIMEOUT_SECONDS`) | After this, batches reject and admin recovery is required |
| Public distribution recovery | 7 days | `creator-pool` (`SelfRecoverDistribution`) | Anyone may restart a stalled distribution after this |
| Admin config changes | 48 h | factory / router / expand-economy timelocks | Every propose→apply pair needs calendared two-step execution |

> The 120 s pool gate is `UPDATE_INTERVAL + 60 s` grace. **Run oracle
> keepers at a 65–75 s cadence** — a single missed round leaves ~60 s of
> slack before commits fail; two missed rounds is an outage.

## The three recurring jobs (the oracle pipeline)

### 1. Pyth pusher
The factory only trusts Pyth data younger than 300 s (and older than a
10 s anti-MEV floor). Someone must keep submitting VAAs to the Pyth
contract. On mainnet, run **Pyth's official `price-pusher`** against
mainnet Hermes — it handles fee bumps, deviation-triggered pushes, and
retries. Budget: Pyth update fee + gas, continuously, forever. Do not
assume another protocol on the chain pushes your feed.

### 2. Oracle keeper (`update_oracle_price`)
Permissionless, rate-limited to one publishing round per 60 s. If it
stops, the published price ages past 120 s and every price-dependent
path reverts. Run `keepers/` (`npm run oracle-keeper`) at 65–75 s
cadence with retry. **Fund `SetOracleUpdateBounty`** so independent
keepers get paid for successful rounds — your bots are the primary, the
bounty market is the redundancy layer.

### 3. Anchor activity
The oracle only publishes when the anchor pool's price accumulator
advanced between snapshots — a round with zero anchor swaps records a
snapshot but publishes nothing, and enough quiet rounds in a row means
staleness. Organic volume normally provides this. During dead hours
either (a) accept fail-closed during inactivity — the code treats this
as intended pressure — or (b) run a small heartbeat bot doing
alternating micro-swaps on the anchor pool. The heartbeat costs gas +
LP commission on tiny notional; recognize it as an ongoing subsidy
masking low liquidity, and make its discontinuation a conscious
decision, not an accident.

## Event-driven keeper duties

Both live in `keepers/` (`npm run distribution-keeper`) and now
**auto-discover every commit pool from the factory's `pools` registry
query** — leave `POOL_ADDRESSES` unset and new pools are picked up
automatically; set it only to pin a subset.

- **`continue_distribution`** — a threshold cross pays nobody in the
  crossing tx; recipients are flushed in gas-budgeted batches by keeper
  calls until `distribution_state` clears. Fund `SetDistributionBounty`
  for third-party backstop. Alert on `distribution_state.is_stalled`
  (24 h timeout → `RecoverPoolStuckStates` from the factory admin;
  after 7 days anyone can `self_recover_distribution`).
- **`retry_factory_notify`** — the factory notification at threshold
  cross is deliberately deferred-on-error. The keeper checks
  `factory_notify_status` and retries while `pending: true`
  (permissionless, idempotent — the factory's `POOL_THRESHOLD_MINTED`
  gate makes double-mints impossible). Retries that keep failing are an
  ops page; the classic root cause is a misconfigured expand-economy.

## The passive contracts

**Router** — no bot needed. Put its admin behind a multisig, and
monitor for unexpected `propose_config_update` events (48 h timelock
gives you the reaction window). The three issues found in deployment
testing are fixed in this repo: simulation now resolves each hop
against the factory registry (standard-pool hops work), pool
simulations quote from `POOL_STATE` accounting reserves instead of
contract balances (no more optimistic quotes), and the router pins each
hop's `max_spread` to the pools' 5% hard cap so `minimum_receive` is
the binding slippage control.

**Expand-economy** — two duties:
1. It must be instantiated with `factory_address` = the **factory
   contract**. The contract now rejects instantiation when
   `factory_address` equals the instantiating wallet (the placeholder
   footgun), and `scripts/verify_deploy.sh` cross-checks the wiring in
   both directions. Correct deploy order: factory first
   (`bluechip_mint_contract_address: null`) → expand-economy pointing
   at the factory → factory `ProposeConfigUpdate` to set the mint
   address → 48 h later, apply — **before** opening pool creation.
2. Keep it funded in bluechip. Every threshold cross pays rewards from
   its balance (up to ~500 BC per cross at default config, decaying);
   if it runs dry, crossings start deferring notifies. Alert on its
   balance like keeper gas.

## Infrastructure rules

- **Supervision, not terminals.** Every bot under systemd
  (`Restart=always`) or a k8s deployment. A keeper that dies with an
  SSH session means a stale oracle within ~2 minutes.
- **One dedicated key per bot**, never shared with admin/treasury —
  two processes signing with one key produce account-sequence races.
  Keep balances low, auto-refill from treasury, alert below threshold.
- **RPC redundancy**: primary + fallback endpoints in every bot;
  ideally run your own node.
- **Post-deploy verification**: run `scripts/verify_deploy.sh` after
  every deploy and after every timelocked config change lands.

## Monitoring

**The single best health probe:** query
`{"internal_blue_chip_oracle_query":{"get_bluechip_usd_price":{}}}` on
the factory every minute; page if it errors **or** the returned
`timestamp` is older than 120 s. Because the system fails closed, this
one probe transitively proves the Pyth pusher, the oracle keeper, and
anchor activity are all alive. (`verify_deploy.sh` performs exactly
this check; the block explorer's ops strip surfaces it too.)

Secondary alerts:
- Pyth `publish_time` age at the Pyth contract
- per-bot wallet balances
- expand-economy bluechip balance (`get_balance`)
- any pool with `factory_notify_status.pending == true` for > 1 h
- any pool with `distribution_state.is_stalled == true`
- unexpected `propose_config_update` events on factory / router /
  expand-economy (the 48 h timelock is your reaction window)

## Reference topology

Two oracle-keeper bots in different regions + funded on-chain bounties
as the third layer; one official Pyth price-pusher; one
distribution/notify watcher (auto-discovering); the price-query canary
in your monitoring stack. Four small processes and a dashboard.

## Governance hygiene

Factory admin, router admin, expand-economy owner → multisig. Every
admin action is 48 h propose→apply: calendar both steps, monitor the
pending-proposal state between them, and treat an unexpected pending
proposal as an incident.
