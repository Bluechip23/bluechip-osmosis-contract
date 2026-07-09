// Contract message shapes that mirror the on-chain definitions in
// factory/src/msg.rs and pool/src/msg.rs. Keep these in sync with the
// Rust side if those evolve.

// ---------------------------------------------------------------------------
// Execute messages — the only thing the keeper actually constructs
// ---------------------------------------------------------------------------

export const PoolExecContinueDistribution = { continue_distribution: {} } as const;
/**
 * Pool-side recovery: re-sends NotifyThresholdCrossed to the factory when
 * the original SubMsg landed in reply_on_error during the threshold-cross
 * commit. Permissionless — anyone may call. Idempotent on the factory
 * side via POOL_THRESHOLD_CROSSED (the factory records the crossing once
 * and rejects duplicates), so a stale/redundant call wastes the caller's
 * gas but cannot double-record.
 */
export const PoolExecRetryFactoryNotify = { retry_factory_notify: {} } as const;
/**
 * Factory-side storage hygiene. Iterates the
 * per-address rate-limit maps and removes entries older than 10× the
 * cooldown window. `batch_size` caps work per call (default 100, hard
 * cap 500 on the contract side) so a large backlog doesn't exceed
 * block gas limits in a single tx. Permissionless on the contract.
 */
export function factoryExecPruneRateLimits(batchSize: number) {
  return { prune_rate_limits: { batch_size: batchSize } } as const;
}

// ---------------------------------------------------------------------------
// Query messages — for read-only checks before deciding to send a tx
// ---------------------------------------------------------------------------

/**
 * Pool query: returns `{ pending: bool }`. True when the pool's
 * threshold cross succeeded but the factory-notify SubMsg landed on
 * reply_on_error. We poll this from the retry keeper to decide
 * whether RetryFactoryNotify is worth dispatching.
 */
export const PoolQueryFactoryNotifyStatus = { factory_notify_status: {} } as const;

/** Wire-format mirror of `creator-pool::msg::FactoryNotifyStatusResponse`. */
export interface FactoryNotifyStatusResponse {
  pending: boolean;
}

// ---------------------------------------------------------------------------
// Error sniffing — contract errors surface as strings in tx responses
// ---------------------------------------------------------------------------

// Substring markers for contract errors the keeper should treat as a
// normal "no-op" rather than a real failure. We match substrings rather
// than exact strings because the cosmjs error wrapper varies across
// versions and chain forks.
// Match both the Rust variant name AND the user-facing #[error(...)] display
// string that propagates over RPC — the CosmWasm client sees the display
// form, not the variant name. Keeping both protects against either form
// appearing in future error payloads.
const SKIP_MARKERS = [
  "NothingToRecover",
  "not found",
  // RetryFactoryNotify: pool returns this when no notify is pending.
  // Expected in normal operation — most pools don't have a pending
  // notify most of the time — so treat as a clean skip rather than an
  // error.
  "No pending factory notification to retry",
  // RetryFactoryNotify: factory rejects when POOL_THRESHOLD_CROSSED is
  // already set (idempotency gate). Means the previous notify actually
  // landed and the pool's pending flag is just stale; the next round
  // of activity will clear it.
  "Threshold crossing already recorded for this pool",
] as const;

export function isExpectedSkipError(message: string): boolean {
  return SKIP_MARKERS.some((m) => message.includes(m));
}
