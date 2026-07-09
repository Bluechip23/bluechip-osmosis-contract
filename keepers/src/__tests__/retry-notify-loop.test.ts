import { beforeEach, describe, expect, it } from "vitest";
import { MockContracts } from "./mock-contracts.js";
import {
  checkAndRetryPool,
  runRetryNotifySweep,
} from "../lib/retry-notify-loop.js";

const FACTORY = "bluechip1factory";
const KEEPER = "bluechip1keeper";
const POOL_A = "bluechip1pool_a";
const POOL_B = "bluechip1pool_b";
const POOL_C = "bluechip1pool_c";

describe("retry-notify keeper", () => {
  let mock: MockContracts;

  beforeEach(() => {
    mock = new MockContracts(KEEPER, { factoryAddress: FACTORY });
  });

  it("skips a pool whose FactoryNotifyStatus reports pending=false", async () => {
    // Default state: no pending. Most pools, most of the time.
    const outcome = await checkAndRetryPool(mock, POOL_A);
    expect(outcome.kind).toBe("skipped");
    if (outcome.kind === "skipped") {
      expect(outcome.reason).toBe("not_pending");
    }
    // Critical: the keeper must NOT dispatch RetryFactoryNotify on a
    // pool that doesn't need it. The contract handler would reject
    // with the canonical "No pending factory notification to retry"
    // error, which would still be classified as a skip on our side
    // — but it would consume gas. The query-first approach saves
    // every wasted-gas tx in the steady state.
    const retryCalls = mock.calls.filter(
      (c) => c.contract === POOL_A && "retry_factory_notify" in c.msg,
    );
    expect(retryCalls).toHaveLength(0);
  });

  it("dispatches RetryFactoryNotify when pending=true", async () => {
    mock.setPendingFactoryNotify(POOL_A, true);
    const outcome = await checkAndRetryPool(mock, POOL_A);
    expect(outcome.kind).toBe("retried");
    if (outcome.kind === "retried") {
      expect(outcome.txHash).toMatch(/^TX/);
    }
  });

  it("treats a stale pending flag (already recorded on factory) as a clean skip", async () => {
    // Race: query said pending=true, but the factory's
    // POOL_THRESHOLD_CROSSED idempotency gate rejects because the
    // crossing was already recorded. The contract's "Threshold crossing
    // already recorded for this pool" error is in the SKIP_MARKERS
    // list, so the keeper classifies it as a skip rather than an error.
    mock.setPendingFactoryNotify(POOL_A, true);
    mock.failNextRetryNotify(POOL_A, true);

    const outcome = await checkAndRetryPool(mock, POOL_A);
    expect(outcome.kind).toBe("skipped");
    if (outcome.kind === "skipped" && outcome.reason === "tx_skip") {
      expect(outcome.detail).toContain("Threshold crossing already recorded");
    } else {
      throw new Error(`expected tx_skip outcome, got ${JSON.stringify(outcome)}`);
    }
  });

  it("reports query_failed when the read errors and does not dispatch a tx", async () => {
    mock.failNextQuery(POOL_A, "RPC: connection reset");

    const outcome = await checkAndRetryPool(mock, POOL_A);
    expect(outcome.kind).toBe("query_failed");
    if (outcome.kind === "query_failed") {
      expect(outcome.detail).toContain("RPC");
    }
    // No execute call should have fired — the keeper must never dispatch
    // a tx on the back of a failed query (would waste gas on a pool
    // whose pending state we don't actually know).
    const retryCalls = mock.calls.filter((c) => "retry_factory_notify" in c.msg);
    expect(retryCalls).toHaveLength(0);
  });

  it("sweep continues past per-pool failures and aggregates totals", async () => {
    mock.setPendingFactoryNotify(POOL_A, true);
    // POOL_B is healthy, no pending.
    mock.setPendingFactoryNotify(POOL_C, true);
    mock.failNextRetryNotify(POOL_C, true); // simulate idempotency race

    const result = await runRetryNotifySweep(mock, [POOL_A, POOL_B, POOL_C]);

    // Order preserved.
    expect(result.outcomes).toHaveLength(3);
    expect(result.outcomes[0]?.pool).toBe(POOL_A);
    expect(result.outcomes[1]?.pool).toBe(POOL_B);
    expect(result.outcomes[2]?.pool).toBe(POOL_C);

    expect(result.outcomes[0]?.kind).toBe("retried");
    expect(result.outcomes[1]?.kind).toBe("skipped");
    expect(result.outcomes[2]?.kind).toBe("skipped"); // tx_skip

    expect(result.totals).toEqual({
      retried: 1,
      skipped: 2,
      queryFailed: 0,
      errored: 0,
    });
  });

  it("clears the pending flag after a successful retry so the next sweep is a no-op", async () => {
    mock.setPendingFactoryNotify(POOL_A, true);

    const first = await runRetryNotifySweep(mock, [POOL_A]);
    expect(first.totals.retried).toBe(1);

    const second = await runRetryNotifySweep(mock, [POOL_A]);
    expect(second.totals.retried).toBe(0);
    expect(second.totals.skipped).toBe(1);
    // Specifically: not_pending, not tx_skip (the contract didn't even
    // get called the second time).
    const o = second.outcomes[0];
    expect(o?.kind).toBe("skipped");
    if (o?.kind === "skipped") expect(o.reason).toBe("not_pending");
  });
});
