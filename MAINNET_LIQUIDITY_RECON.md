# Mainnet Liquidity Recon — the oracle substrate (read-only, osmosis-1)

**What this is:** a read-only survey of *actual* Osmosis mainnet (`osmosis-1`)
pool liquidity for the pricing set we discussed, to answer the one question the
local tests could not: **are the real pools deep enough for the median/TWAP
oracle to be safe?**

**Method:** pool balances pulled from the Osmosis SQS snapshot and then
**ground-truth-verified against on-chain `x/bank` balances via LCD** (every
figure below was cross-checked; SQS matched chain exactly). USD prices from
CoinGecko. **No transactions, no deploy, no mainnet env touched.**

**Snapshot caveat:** this is a point-in-time read. USDC/USDT/DAI-side figures
are dollar amounts of real stablecoin tokens and are price-independent; the
OSMO-side figures use a live OSMO price (~$0.032 at snapshot, corroborated by
the balanced GAMM pool-1 ratio). Re-run before acting.

---

## The finding, up front

**On current mainnet liquidity, no configuration of these pools yields a safe
USD oracle.** The entire OSMO↔USD substrate has collapsed to **single-digit
thousands of dollars per venue**. The multi-pool median is only as strong as
its ⌈n/2⌉-th-cheapest source, and by the measured cost law (~12% of the
constraining side moves a pool +25%) that number is **~$1,000–$2,000**. So the
whole oracle — threshold valuation *and* airdrop-weight math — can be **controlled
or bricked for roughly $1,500–$3,000**, regardless of how the sources are
arranged. This is a **substrate problem, not a contract problem**: the contracts
are correct (16 E2E tests); there is simply no deep OSMO/USD liquidity to price
against right now.

---

## The real pools (ground-truth verified)

### Direct stable anchors (OSMO vs a USD stable)

| Pair | Best pool | Stable-side depth (real $) | ~Cost to move +25% |
|---|---|---:|---:|
| **OSMO/USDC** (Noble) | 1464 (CL) | **$3,795** | ~$455 |
| **OSMO/DAI** | 674 (GAMM) | **$8,765** (18-dec) | ~$1,041 |
| **OSMO/USDT** (alloyed) | 1857 | **~$7 — effectively dead** | ~$1 |

`OSMO/USDT has no liquidity` — so the "add OSMO/USDT for a second stable anchor"
plan from our earlier discussion **is not available on mainnet**. The deepest
stable pair is actually OSMO/**DAI** (pool 674, ~$8.8k, but DAI is 18-decimal
and carries its own peg/venue risk), then OSMO/USDC (~$3.8k).

### Routed sources — cost is `min(leg1, leg2)`

| Source | Leg 1 (OSMO/asset) | Leg 2 (asset/USDC) | **Constraining depth** | ~Cost +25% |
|---|---|---|---:|---:|
| **ATOM** | pool 1: ~$327k | pool 1251: **$16,881** | **$16,881** | ~$2,026 |
| **AKT** | pool 1093: ~$92k | pool 1301: **$10,986** | **$10,986** | ~$1,318 |
| **BTC** (allBTC) | pool 1995: **$250** | pool 1943: $98,765 | **$250** | ~$30 |

The BTC source is a trap: its allBTC/USDC leg is deep ($98.8k) but its
**OSMO/allBTC leg holds only $250**, so the whole source costs ~$30 to corrupt.
Including it would hand the attacker a nearly-free vote. This is exactly the
"a routed source costs min(leg depths), not the headline pair's depth" rule the
local tests flagged — now with real numbers proving it bites.

### Every viable source, ranked by attack cost

| # | Source | Constraining depth | ~Cost to move +25% | Usable? |
|---|---|---:|---:|---|
| 1 | ATOM routed | $16,881 | **~$2,026** | ✅ strongest |
| 2 | AKT routed | $10,986 | ~$1,318 | ✅ |
| 3 | OSMO/DAI direct | $8,765 | ~$1,041 | ⚠ 18-dec, DAI risk |
| 4 | OSMO/USDC direct | $3,795 | ~$455 | ⚠ weak, but forced (see below) |
| 5 | BTC routed | $250 | ~$30 | ❌ exclude |
| — | OSMO/USDT direct | ~$7 | ~$1 | ❌ dead |

**Best realistic set** = {USDC, DAI, ATOM, AKT} — 4 sources (even). Breaking an
even set needs only half: the two cheapest (USDC ~$455 + DAI ~$1,041) ≈
**$1,500 to deadlock/break the oracle**; add AKT (~$1,318) ≈ **$2,800 to own it
outright**. There is no viable 5th source to make it an odd, expensive-majority
set — BTC and USDT are both effectively free votes.

**Extra constraint:** the primary `pricing_pool_id` is also the mandatory
cross-denom **fee-swap route** at threshold crossing (it must trade
OSMO/`usd_quote_denom`). That forces the primary to be the OSMO/USDC pool 1464 —
the ~$3.8k one — so the thinnest stable pool is load-bearing for *both* pricing
and the crossing's fee acquisition.

---

## Why the median can't save this

The median defends against a *minority* of thin/corrupted sources by leaning on
a *majority* of deep ones. That requires deep ones to exist. Here **every**
source is thin, so there is no expensive majority to hold the line — the
⌈n/2⌉-th source is ~$1k. This is the R2-E "no depth gate" finding realized: the
contract treats a $250 pool and a $17k pool as equal votes, and on this
substrate that is the difference between a $30 and a $2,000 attack — both
trivial.

Mitigations present but insufficient at this liquidity:
- **TWAP window** (raise 600→800→max 3600s): forces the attacker to *hold* the
  manipulation for the window against arbitrage. But thin pools attract little
  arbitrage capital, and holding a ~$2k position for an hour is cheap. Helps at
  the margin; does not close a 100×-too-thin gap.
- **RATE_MAX $10k ceiling:** only stops pushing OSMO *above* $10k. An attacker
  inflating OSMO from $0.03 to $0.10 (3×) stays far under the ceiling and still
  badly distorts valuations. Lowering RATE_MAX toward ~3× spot would help but is
  a blunt instrument.
- **Deviation filter / quorum:** fail *safe* (no mispricing) but convert a
  cheap manipulation into a cheap **liveness DoS** — halting all commits.

---

## Options (honest — pick by economic model, not by "is it secure")

1. **Denominate the threshold in OSMO (native), not USD.** Removes the oracle
   dependency entirely — no pricing pool, no manipulation surface. Cleanest fix
   if a native-denominated threshold is acceptable to the economics.
2. **Use an external price oracle (Pyth / Band) for OSMO/USD** instead of
   on-chain pool TWAP. Deeper, cross-venue, far harder to move for $2k. Larger
   design change (new dependency, new trust assumption).
3. **Gate on attack *profit*, not just cost.** F-4's gain = stolen airdrop
   weight × token value. For a **$20** threshold pool the dollar value an
   attacker can extract may be *below* the ~$2k attack cost — in which case the
   thin oracle is economically (not technically) safe *for small pools*. This
   needs a concrete per-pool computation: expected airdrop $-value at stake vs
   ~$1.5k–$3k attack cost. Do not hand-wave it.
4. **If launching on pool TWAP anyway:** set `twap_window_seconds = 3600`,
   configure {USDC(primary/1464), DAI(674, quote_decimals=18), ATOM(1/1251),
   AKT(1093/1301)}, `min_valid_sources = 3`, a tight `max_deviation_bps`
   (e.g. 1000–1500), **exclude BTC and USDT**, and treat the oracle as good only
   for thresholds small enough that option 3 holds. Monitor OSMO liquidity
   (RUNBOOK alarm) and be ready to migrate pools as depth moves.
5. **Wait for liquidity to recover** before shipping USD-threshold pools.

---

## Pool-id reference (verified on-chain, osmosis-1)

```
OSMO/USDC  (Noble)   pool 1464  CL    USDC  = ibc/498A0751…B876BA6E4 (6dec)
OSMO/DAI             pool 674   GAMM  DAI   = ibc/0CD3A028…1D866DF7  (18dec)
OSMO/USDT (alloyed)  pool 1857  CL    (dead ~$7) allUSDT = factory/osmo1em6xs…/alloyed/allUSDT
OSMO/ATOM            pool 1     GAMM  ATOM  = ibc/27394FB0…5F41E5EB2 (6dec)  [$327k, deepest pool on-chain]
ATOM/USDC            pool 1251  CL    → the ATOM routed source's USD leg ($16.9k)
OSMO/AKT             pool 1093  CL    AKT   = ibc/1480B8FD…64743EF4  (6dec)  [$92k]
AKT/USDC             pool 1301  CL    → the AKT routed source's USD leg ($11.0k)
OSMO/allBTC          pool 1995  CL    allBTC= factory/osmo1z6r6q…/alloyed/allBTC (8dec) [$250 — EXCLUDE]
allBTC/USDC          pool 1943  CL    ($98.8k, but leg-1 kills it)
```

Deepest OSMO venue overall is **OSMO/ATOM (pool 1, ~$327k)** — but it prices OSMO
in ATOM, not USD, so it only helps as a routed source, bottlenecked by the
$16.9k ATOM/USDC leg.
