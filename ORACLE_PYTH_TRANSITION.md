# Oracle transition: x/twap median → Pyth (assessment + plan)

**Why:** the USD-denominated threshold requires an OSMO→USD price feed on every
commit. The on-chain pool-TWAP substrate is too thin to price safely
(`MAINNET_LIQUIDITY_RECON.md`: whole oracle controllable for ~$1.5–3k). Pyth
aggregates many CEX/DEX venues, so moving its OSMO/USD price costs orders of
magnitude more. The original `bluechip-contracts` repo already used Pyth, so
both halves of the integration exist to port.

---

## Is it an easy transition? — the honest answer

**Contract surface: small and contained (good).** The entire native→USD oracle
lives in one file, `factory/src/usd_price.rs`, exposed through exactly two
factory queries — `ConvertNativeToUsd` and `CommitContext`. Pools NEVER touch
the price source; they query the factory and consume a single `rate_used`
scalar (micro-USD per micro-native, `RATE_PRECISION = 1e6`). So the swap is:
rewrite the internals of `probe_median_usd_rate` to read Pyth instead of
x/twap, and return the same `rate_used`. **The pool side, the response structs,
the crossing math, and the router are all unchanged.**

**But it reintroduces a keeper (the real cost).** Pyth on Cosmos is
push-based: the contract stores the last price *someone* pushed. I queried the
**mainnet** Pyth contract
(`osmo13ge29x4e2s63a8ytz2px8gurtyznmue4a69n5275692v3qn3ks8q7cwck7`, code 142)
for OSMO/USD (feed `5867f5683c757393a0670ef0f701490950fe93fdb006d181c8265a831ac0c5c6`):

```
price = 3037131, expo = -8  → $0.03037 per OSMO   (corroborates the depth recon)
publish_time = 1774953351   → ~115 DAYS stale at time of check
```

**Nobody is keeping OSMO/USD fresh on Osmosis.** So a keeperless "read + reject
if stale" design would fail closed on every commit. **You must run a price
keeper** that fetches a signed OSMO/USD update from Pyth Hermes and submits
`UpdatePriceFeeds` to the Pyth contract, then the commit reads it — exactly the
push model the original repo used (`keepers/`, the docs' `tx1: UpdatePriceFeeds`
/ `tx2: Commit` ordering).

Net tradeoff, stated plainly:

| | x/twap median (current) | Pyth + keeper |
|---|---|---|
| Manipulation cost | **~$1.5–3k** (too cheap) | **very high** (aggregated venues) |
| Freshness | always fresh, on-chain, keeperless | needs a keeper pushing every N s |
| Liveness dependency | none (RUNBOOK: "nothing to keep alive for prices") | **a price keeper** joins the distribution keeper |
| Failure mode | mispricing under manipulation | fail-closed if keeper lags past staleness gate |

You are trading a keeperless-but-manipulable oracle for a
manipulation-resistant-but-keeper-dependent one. For a USD threshold that must
be secure, that is the correct trade — but the RUNBOOK's "no price liveness"
claim becomes false and must be rewritten.

---

## What you already have to port (from `Bluechip23/bluechip-contracts`)

- `factory/src/pyth_types.rs` — the Pyth CW wire shapes, including the
  **asymmetric JSON encoding** gotcha (`price`/`conf` are JSON strings,
  `expo`/`publish_time` are JSON numbers). Portable ~verbatim. This is the part
  that's annoying to get right and it's already done.
- `internal_bluechip_price_oracle*.rs` — the live Pyth read with a
  **confidence-interval gate** (`pyth_conf_threshold_bps`, reject if
  `conf/price` too wide) and **staleness gate** (`MIN_PYTH_AGE` /
  `MAX_PRICE_AGE_SECONDS_BEFORE_STALE = 300s`), plus the expo→scale conversion.
- `keepers/src/oracle-keeper.ts` — the push keeper (Hermes → `UpdatePriceFeeds`).
- The admin knobs + bounds for the conf/staleness gates.

Note the original anchored on ATOM/USD and derived bluechip price through pools;
here it is simpler — read **OSMO/USD** directly (native denom = uosmo) and return
`rate_used`. Drop the pool-derivation layer entirely.

---

## Concrete plan (contract side)

1. **New `usd_price.rs` internals.** Replace `probe_median_usd_rate` body with
   `probe_pyth_usd_rate`: smart-query the configured Pyth contract for the
   OSMO/USD feed, apply the staleness gate (`env.block.time - publish_time ≤
   max_staleness`, fail closed), apply the confidence gate
   (`conf * 10_000 / price ≤ conf_bps`, fail closed), convert
   `price × 10^expo` into `RATE_PRECISION` micro-USD-per-micro-native, and run
   the existing `apply_rate_sanity` (zero / dust / `RATE_MAX`). Keep the exact
   `rate_used` semantics → zero pool-side change.
2. **Config.** Add `pyth_contract_addr`, `osmo_usd_feed_id`,
   `max_pyth_staleness_seconds`, `pyth_conf_threshold_bps` to
   `FactoryInstantiate`; keep them behind the 48h timelock + a live probe at
   propose/apply (same pattern as the pricing-pool probe today). The
   `MultiOracleConfig` median fields become vestigial — leave `#[serde(default)]`
   for back-compat or remove in a clean cut.
3. **Keep `pricing_pool_id` as a FEE-EXECUTION route only.** The crossing still
   swaps OSMO→USDC to pay the ~20-USDC poolmanager creation fee, routed through
   `pricing_pool_id` (`threshold_payout.rs`). Pyth gives the *price* to size the
   swap budget (`usd_to_native_at_rate`), but a real pool is still needed to
   *acquire* the USDC. Swapping ~$20 through the $3.8k OSMO/USDC pool is 0.5% of
   depth — fine for execution even though its TWAP is no longer trusted for
   pricing. So: pool stays, its price is no longer believed.
4. **Docs/ops.** Rewrite the RUNBOOK price section: add the price keeper
   (Hermes → `UpdatePriceFeeds`), its cadence vs the staleness gate, its gas
   wallet, and a "feed age" alarm. The canary probe (`ConvertNativeToUsd`) still
   works and now also catches a lagging keeper (fail-closed on staleness).
5. **Tests.** The osmosis-test-tube harness can store the real Pyth CW contract
   (code is public) OR a mock oracle (the original repo ships `mockoracle/`),
   push a price, and assert `ConvertNativeToUsd` returns it, that a stale feed
   fails closed, and that a wide-confidence feed fails closed. The crossing/
   atomicity/router suites are unaffected (they only see `rate_used`).

---

## Open decision for you

- **Staleness gate vs keeper cadence.** Original used 300s live-staleness with a
  60s update floor. Tighter gate = safer but more keeper pressure / more
  fail-closed risk if the keeper hiccups. Pick the pair together.
- **Scope of the first cut.** Minimal viable: swap `usd_price.rs` to a
  single-feed OSMO/USD Pyth read + staleness + conf gate, port `pyth_types.rs`,
  add config + a mock-oracle test. Everything downstream is untouched.

**Verified facts (read-only, osmosis-1):** Pyth contract
`osmo13ge29x4e2s63a8ytz2px8gurtyznmue4a69n5275692v3qn3ks8q7cwck7` (code 142,
label "pyth") is live; OSMO/USD feed
`5867f5683c757393a0670ef0f701490950fe93fdb006d181c8265a831ac0c5c6` resolves to
$0.03037 but was ~115 days stale ⇒ **a keeper is mandatory**.

---

## Adversarial audit of the Pyth migration (two passes)

The Pyth-migration code was audited twice with fresh eyes, and the test suite
hardened, before finalizing.

**Pass 1 — findings fixed:**
- **A (medium, liveness footgun):** the feed-id match `feed.id != feed_id` was
  case-SENSITIVE while config validation accepts any hex case — a mixed-case
  feed id would fail closed on every read (or block instantiate confusingly).
  → now `eq_ignore_ascii_case`.
- **B (robustness, panic footgun):** `normalize_pyth_price_to_rate` is `pub` and
  would **wasm-trap** on an out-of-range `expo` (`6 − |expo|` underflow /
  `10^n` overflow). Safe in the probe path (gated first) but unsound as a public
  fn. → internal `[-12,-4]` guard makes it TOTAL (fail-closed `Err`, never a trap).
- **C (cleanup):** dropped an unused `_storage` param
  (`load_pyth_conf_threshold_bps` → `effective_pyth_conf_bps(config)`).
- **E (docs):** `AUDIT_FINDINGS_E2E.md` (the median-oracle report) got a
  SUPERSEDED header so it isn't read as the current design.
- Confirmed **no dangling references** to the removed median oracle in shipping
  code (only the historical report mentions it).

**Pass 2 — fresh re-read + clippy:**
- clippy clean on all Pyth logic (one redundant `as u32` cast removed).
- Verified benign: the conf-gate `saturating_mul` (a near-`i64::MAX` price
  saturates the threshold, but such a price normalizes above `RATE_MAX` and is
  rejected anyway — pinned by a test); integer truncation in the divide branch
  (sub-micro-USD, dust-gated); `conf == 0` passes (perfectly-confident, not a
  bug).
- **Operator-verification items (documented, not code bugs):** a config feed-id
  that is valid-hex but points at the WRONG asset would misprice — but a
  high-value wrong feed (e.g. BTC) is caught by `RATE_MAX` and a near-zero one
  by the dust gate; only a coincidentally-$0-to-$10k wrong feed slips, so verify
  the feed id off-chain (same class as the old pool-id verification). The keeper
  cannot push an unsigned/bad price — the Pyth contract verifies guardian sigs.

**Hardening tests added (12; unit + probe-pipeline):** normalize across the full
`[-12,-4]` expo range (all → same rate for $1.00); out-of-range expo → `Err`
not panic (incl. `i32::MIN/MAX`); conf-gate saturation → `RATE_MAX` fail-closed;
realistic OSMO $0.0319 → 31_935; exact boundary tests for staleness (300 vs 301),
min-age (10 vs 9), future-skew, and conf (threshold vs +1); feed-id mismatch →
fail-closed; feed-id case-insensitivity; expo extremes (-4/-12) via the full
probe; expo out-of-range via the probe.

**Final suite:** factory **127**, workspace **318**, osmosis-test-tube E2E
**11/11** on the optimized bytes, `make check` **3/3**. Final canonical hashes:
factory `5b87590f85bcaa762ce7572b686865f182380015aa1aad33e99d1286f77da431`,
creator_pool `d0778ea3…` and router `ea5c69bb…` (both UNCHANGED — the migration
is factory-contained). Deployed + validated on osmo-test-5
(`TESTNET_VALIDATION_PYTH.md`).
