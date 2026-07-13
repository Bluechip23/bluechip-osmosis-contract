# Fuzzing

Pure-math fuzz targets live in `fuzz/` (excluded from the default
workspace; requires nightly + `cargo-fuzz`):

```bash
cargo +nightly fuzz run fuzz_swap_math       # xyk swap math invariants
cargo +nightly fuzz run fuzz_threshold_check # threshold accounting invariants
```

Coverage status:

- `fuzz_swap_math` exercises the live `pool_core::swap::compute_swap`
  math directly.
- `fuzz_threshold_check` models the threshold-valuation fixed-point
  math approximately; it should be re-pointed at the live
  `factory::usd_price` helpers (`twap_dec_to_rate` / `native_to_usd`)
  and the pool-side inverse (`usd_to_native_at_rate`).

Planned: a stateful property harness around the commit → threshold →
swap / liquidity lifecycle with machine-checked conservation invariants
(ledger sum ≤ threshold, reserve/bank reconciliation, MINIMUM_LIQUIDITY
floor) — the highest-value verification addition before mainnet.
