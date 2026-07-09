# Fuzzing

> **Status (strip-down):** the stateful proptest harness (`fuzz-stateful/`)
> and the expand-economy formula fuzzer were retired together with the
> internal oracle / expand-economy / bluechip-mint plumbing. The harness's
> world model was built around oracle rates and expansion accounting that
> no longer exist. If a stateful harness is wanted again post-launch,
> rebuild it around the simplified surface (commit → threshold → swaps /
> liquidity).

Remaining pure-math fuzz targets live in `fuzz/` (excluded from the
default workspace; requires nightly + `cargo-fuzz`):

```bash
cargo +nightly fuzz run fuzz_swap_math       # xyk swap math invariants
cargo +nightly fuzz run fuzz_threshold_check # threshold accounting invariants
```

Note: `fuzz/` targets may need updating to the native-denominated
threshold API before running.
