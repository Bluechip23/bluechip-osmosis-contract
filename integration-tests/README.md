# integration-tests — osmosis-test-tube end-to-end harness

These tests run the contracts against a **real in-process Osmosis chain**
(via [`osmosis-test-tube`](https://github.com/osmosis-labs/test-tube)), so
they exercise the native modules the unit tests can only stub:
`tokenfactory`, `gamm`, `poolmanager`, and `twap`.

They are the intended pre-testnet gate for everything that mocks can't
prove: real `MsgCreateBalancerPool` seeding (and decoding the response
protobuf), the x/twap fee-route read, TokenFactory mints, the deferred
pool opening after distribution, and `MsgSwapExactAmountIn` routing +
reply forwarding.

## Why this crate is excluded from the workspace

`osmosis-test-tube` links a large Go shared library (the Osmosis chain
binary) and the tests need pre-built optimized wasm. Both are too heavy for
the default `cargo test`, so the crate is listed under `exclude` in the root
`Cargo.toml`. A normal `cargo test` / `cargo test --workspace` at the repo
root neither builds nor runs it.

## Prerequisites

1. **A toolchain that can build `osmosis-test-tube`.** It needs a working Go
   + CGO toolchain the first time it builds the embedded chain, or a
   platform with a prebuilt `libosmosistesttube` (Linux x86_64 / macOS).
   This generally does **not** work inside restricted CI sandboxes — run it
   on a normal dev machine or a full CI runner.

2. **Optimized wasm artifacts** at `../artifacts/`:
   - `artifacts/factory.wasm`
   - `artifacts/pool.wasm`
   - `artifacts/router.wasm`

   Build them with the workspace optimizer (rust-optimizer / cosmwasm
   `optimize` — the same build the deploy tooling uses). For a quick local
   build without Docker you can also do:

   ```bash
   # from the repo root
   RUSTFLAGS='-C link-arg=-s' cargo build --release --target wasm32-unknown-unknown \
     -p factory -p creator-pool -p router
   mkdir -p artifacts
   cp target/wasm32-unknown-unknown/release/factory.wasm     artifacts/factory.wasm
   cp target/wasm32-unknown-unknown/release/creator_pool.wasm artifacts/pool.wasm
   cp target/wasm32-unknown-unknown/release/router.wasm      artifacts/router.wasm
   ```

   > The production artifacts should come from the deterministic optimizer,
   > not a plain `cargo build` — the command above is only for local
   > integration runs.

## Running

```bash
cd integration-tests
cargo +stable test --release -- --test-threads=1 --nocapture
```

(`+stable`: this crate's lockfile needs a newer Cargo than the 1.75
toolchain pinned for the contracts.)

Start with `instantiate_factory_against_live_twap` (proves the x/twap
config probe end to end), then `full_lifecycle_create_commit_cross_swap`
(create → cross → distribute → pool opens → swap).

The nine scenarios: factory instantiate against a live TWAP; the full
lifecycle; a mainnet-shape crossing that buys a USDC creation fee and
pays it when the pool opens; TWAP orientation on a 25:1 pool;
third-party LP join/exit; mid-crossing atomicity; an unroutable fee
denom failing closed and recovering; the router end to end; and a
committer withdrawal from an uncrossed pool after 18 months.

## Version note

The scenario logic uses the contracts' own message/response types, so it
cannot drift from the wire format. The only version-sensitive surface is the
`osmosis-test-tube` runner API, which is confined to:

- the `store_code` / `instantiate` / `execute` / `query` calls,
- `Gamm::create_basic_pool`, `app.increase_time`, and
- the `tt` module at the bottom of `tests/lifecycle.rs` (bank balance query).

Pin `osmosis-test-tube` in `Cargo.toml` to the release that tracks the
Osmosis version you target **and** embeds a `cosmwasm-vm` compatible with
this workspace's `cosmwasm-std` 2.x. If a method name or response shape
differs, adjust those few call sites only.

## What to add next (suggested coverage, in priority order)

- **Over-cap crossing**: lower `max_bluechip_lock_per_pool` below the net
  raise and assert `CREATOR_EXCESS_POSITION` earmark + a successful
  `ClaimCreatorExcessLiquidity` after `increase_time(lock)`, including that
  the claim survives an emergency drain.
- **First-commit-crosses (shortfall)**: cross with a single commit whose 1%
  reserve is below the live native gamm fee; assert the crossing still
  succeeds and the pool later opens with the seed reduced by the shortfall.
- **Slippage revert**: a `SimpleSwap` with a too-tight `belief_price` reverts
  via the `token_out_min_amount` floor; a post-threshold `Commit` without
  `belief_price` is rejected (`BeliefPriceRequired`).
- **Breaker**: drain the native pool's OSMO side (large external swap)
  below 25% of seed and assert the next contract-routed swap reverts with
  `LiquidityBelowSeedFloor` without latching a pause; push the price UP
  instead (creator side below 25%) and assert swaps still go through.
- **Fee change between crossing and pool opening**: change the chain's
  pool-creation fee after the crossing and assert the pool-opening call
  reduces the seed (higher native fee), remits the surplus (lower fee),
  or fails with the "send the missing coins" error (re-denominated fee).
- **Drain before the pool opens**: arm and execute an emergency withdraw
  between crossing and the last batch; assert committers can still claim
  via `ClaimFailedDistribution` and the held fee coin is swept.
- **Router multi-hop**: creatorA → OSMO → creatorB across two seeded pools,
  asserting `minimum_receive` binds end to end.
- **Distribution at scale**: many committers, batched `ContinueDistribution`,
  dust settlement to the creator, and `ClaimFailedDistribution`.
