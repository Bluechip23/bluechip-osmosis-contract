# Testnet Validation — Pyth oracle stack on osmo-test-5

On-chain validation of the audited Pyth-oracle build against the **real
Osmosis testnet** (osmo-test-5). Everything below is a real transaction on a
real chain; tx hashes are cited so each claim is independently checkable.

**Bytes deployed:** the audited optimized wasm (`make check` clean). Factory
code **13280** (hash at deploy `69ea04b1…`; a subsequent clippy-only
redundant-cast cleanup produced the final canonical `5b87590f…` — behaviour
identical, re-verified by the unit + E2E suites). Creator-pool code 13279,
router 13281, mock-pyth 13282.

**Two things constrained the run and are stated up front:**
1. The osmo-test-5 faucet + RPC/LCD were intermittently down (a real chain
   outage, ~12 min mid-run). All ~62 OSMO across the throwaway keyring is the
   entire budget.
2. At the **real** Pyth price (OSMO ≈ $0.032) that ~62 OSMO is worth ~$2 —
   **below the pool's $5 minimum pre-threshold commit**, and the $5 minimum is
   only lowerable via a 48h-timelocked config update. So the full
   *crossing/distribution* lifecycle cannot run against the real price on this
   testnet. It was therefore run against a **mock Pyth contract** set to a
   workable price ($3.00), clearly labelled below. The **oracle integration
   itself** was validated against the **real** Pyth contract + real signed
   prices.

---

## Part A — REAL Pyth oracle, end to end (the new code, on the real chain)

Factory `osmo16d2uvdu2gzax4gukew5cs653gfctmwm4f3uft746rwycmsmsq9tqlml0v7`
pointed at the **real** testnet Pyth contract
`osmo1lltupx02sj99suakmuk4sr4ppqf34ajedaxut3ukjwkv6469erwqtpg9t3`, OSMO/USD
feed `d9437c19…c322b857`.

| # | What | Result | tx / evidence |
|---|---|---|---|
| A1 | **Real Pyth price push** (beta Hermes → `update_price_feeds`) | feed went **115 days stale → 71 s fresh**; guardian sig verified on-chain | `801F4CFA…` (code 0, h65921183) |
| A2 | **Factory reads the real Pyth price** at instantiate live-probe | `ConvertNativeToUsd` = **$0.0318** (rate_used 31750) | deploy readback |
| A3 | **MIN_PYTH_AGE anti-MEV gate** (push then commit same window) | commit rejected **"price too fresh: age <10s"** | bob's first commit sim |
| A4 | **Minimum-commit gate** | commit rejected **"Commit too small: minimum $5.00"** | bob commit sim |
| A5 | **Stale → FAIL-CLOSED** (let feed age past 300 s) | `ConvertNativeToUsd` = **"stale … age 331s exceeds max 300s"** — every commit would revert | query |
| A6 | **Recovery** (re-push) | `ConvertNativeToUsd` = **$0.031749** again | `8FF79BF3…` |

This is the load-bearing result: the production oracle path — a keeper relaying
Pyth Hermes updates → the real Pyth CW contract verifying guardian signatures →
the factory reading the fresh price and failing closed when it goes stale —
**works on a real chain.**

---

## Part B — Full crossing + distribution mechanics (mock price, real chain)

Because real-price economics blocks a crossing (Part A note 2), the mechanics
were run on factory
`osmo1wtv58vx39xfwwmv4lxtcq83jpmt24n2mu3ceftgerxx6qlzwp77sqt5rzr` (same code
13280) pointed at a **mock Pyth contract**
`osmo1lq4tzvwlwc4f03mfvggdpskfs9045q4yx5e2nhna9g8kp0hr063q9w8209` set to
**OSMO = $3.00**, threshold **$12**. The mock has the identical wire shape to
real Pyth, so the factory code path is unchanged — only the price source's
freshness is operator-controlled.

Pool `osmo1ffne7l0ahnz3vxlvna9d648n78x2fr4pyst3ecsn9yau04yrva3sukr3rr`
(denom `factory/…/mechdemo`).

| # | Step | Result | tx |
|---|---|---|---|
| B1 | bob pre-commits 1.7 OSMO | credited **exactly $5.10** (1.7 × $3, Pyth-valued) | `669000FF…` |
| B2 | carol pre-commits 1.7 OSMO | pool raised **$10.20 / $12** | `F502587B…` |
| B3 | **alice crosses** (1.7 OSMO) | pool **`fully_committed`**; **native GAMM pool 1343 created** (POOL_ID stored from reply); gas 1,063,504 | `F849217A…` |
| B4 | seed check | pool 1343 holds **350B MECHDEMO + 2.811 OSMO** (the pool_seed mint + net OSMO) | gamm query |
| B5 | **distribution** (`continue_distribution`) | full **1,200,000,000,000** supply minted; state reaped | `42D2F18C…` |
| B6 | **pro-rata payout** — EXACT, incl. crosser-ledgered-less | see table | LCD balances |
| B7 | **F-1 belief gate** — null-belief SimpleSwap | **rejected** ("belief_price is required…") | sim |
| B8 | belief-priced SimpleSwap | **succeeds** (code 0) — post-threshold trading works | `D028AF9A…` |

**B6 pro-rata (the audited CLEAN-3 crosser nuance, proven on-chain):**

| Holder | MECHDEMO | Derivation |
|---|---|---:|
| bob (pre-committer) | **212.5B** | 500B × $5.10 / $12 |
| carol (pre-committer) | **212.5B** | 500B × $5.10 / $12 |
| alice (crosser + creator) | **425B** | 350B creator+bluechip rewards + 500B × **$1.80** / $12 |

The crosser (alice) is ledgered only for her **$1.80 value-to-threshold**
(= $12 − $10.20 already raised), not her full $5.10 — so she receives a *smaller*
commit-return share than the pre-committers, exactly as the distribution math
specifies. Totals: 212.5 + 212.5 + 425 = 850B to holders + 350B seeded = **1.2T**.

---

## What this proves vs. what the E2E already covers

**Proven on the real testnet chain (this pass):** audited-bytes deploy; the
real Pyth oracle push/read/gates/stale-fail-closed/recovery; a real 3-committer
threshold crossing with atomic mint + native GAMM pool creation; exact pro-rata
distribution including the crosser nuance; and the F-1 belief-price attack gate.

**Covered by the osmosis-test-tube E2E** (real Osmosis gamm/poolmanager/
tokenfactory/twap modules, on the optimized bytes — 11/11, plus 318 workspace
unit tests): crossing atomicity under forced mid-chain revert; the router 48h
timelock + `minimum_receive` + F-5 no-wedge; emergency-withdraw arc;
third-party GAMM LP join/exit; cross-denom USDC fee crossing. These were not
re-burned on the flaky testnet because they are deterministically proven there.

**Not achievable on this testnet:** a crossing at the *real* Pyth price (needs
~157 OSMO for one $5 minimum commit; faucet down). This is an economics/funding
limit, not a code limit.

---

## Key addresses (osmo-test-5)

```
codes:            creator_pool 13279 · factory 13280 · router 13281 · mock_pyth 13282
real-Pyth factory osmo16d2uvdu2gzax4gukew5cs653gfctmwm4f3uft746rwycmsmsq9tqlml0v7
router            osmo14n2uc7a3r4avxdag77g3kvftz7gtju7qnamnzjuc0mv5r0d7zqmqv0n4f3
testnet Pyth      osmo1lltupx02sj99suakmuk4sr4ppqf34ajedaxut3ukjwkv6469erwqtpg9t3
OSMO/USD feed     d9437c194a4b00ba9d7652cd9af3905e73ee15a2ca4152ac1f8d430cc322b857  (beta Hermes)
mechanics factory osmo1wtv58vx39xfwwmv4lxtcq83jpmt24n2mu3ceftgerxx6qlzwp77sqt5rzr  (mock oracle)
mock-pyth         osmo1lq4tzvwlwc4f03mfvggdpskfs9045q4yx5e2nhna9g8kp0hr063q9w8209
mechanics pool    osmo1ffne7l0ahnz3vxlvna9d648n78x2fr4pyst3ecsn9yau04yrva3sukr3rr  → gamm/pool/1343
```
