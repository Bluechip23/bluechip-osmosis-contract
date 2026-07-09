import { beforeEach, describe, expect, it } from "vitest";
import { drainPool, runDistributionSweep } from "../lib/distribution-loop.js";
import { MockContracts } from "./mock-contracts.js";

const FACTORY = "bluechip1factory";
const POOL_A = "bluechip1poolA";
const POOL_B = "bluechip1poolB";
const POOL_C = "bluechip1poolC";
const KEEPER = "bluechip1keeper";

describe("distribution keeper integration", () => {
  let mock: MockContracts;

  beforeEach(() => {
    mock = new MockContracts(KEEPER, { factoryAddress: FACTORY });
  });

  it("drains an actively distributing pool across multiple batches", async () => {
    // Pool has 5 batches of work queued.
    mock.setupPoolDistribution(POOL_A, 5);

    const result = await drainPool(mock, POOL_A, /* perCallDelayMs */ 0);

    expect(result.madeProgress).toBe(true);
    expect(result.batches).toBe(5);
    expect(result.complete).toBe(true);
    expect(result.lastOutcome).toEqual({ kind: "tx", outcome: { kind: "ok" } });
  });

  it("returns early when a pool is not distributing (NothingToRecover)", async () => {
    // No setup — pool is not in distribution state.
    const result = await drainPool(mock, POOL_A, 0);

    expect(result.madeProgress).toBe(false);
    expect(result.batches).toBe(0);
    expect(result.complete).toBe(false);
    expect(result.lastOutcome).toEqual({ kind: "not_running" });
  });

  it("issues exactly one ContinueDistribution tx per batch", async () => {
    mock.setupPoolDistribution(POOL_A, 10);
    const result = await drainPool(mock, POOL_A, 0);

    expect(result.batches).toBe(10);
    const continueCalls = mock.calls.filter(
      (c) => c.contract === POOL_A && "continue_distribution" in c.msg,
    );
    expect(continueCalls).toHaveLength(10);
  });

  it("sweep skips non-distributing pools, drains the one that is", async () => {
    mock.setupPoolDistribution(POOL_B, 3);
    // POOL_A and POOL_C are not distributing.
    const pools = [POOL_A, POOL_B, POOL_C];

    const sweep = await runDistributionSweep(mock, pools, 0);

    expect(sweep.madeProgress).toBe(true);
    expect(sweep.pools[POOL_A]?.lastOutcome).toEqual({ kind: "not_running" });
    expect(sweep.pools[POOL_B]?.batches).toBe(3);
    expect(sweep.pools[POOL_B]?.complete).toBe(true);
    expect(sweep.pools[POOL_C]?.lastOutcome).toEqual({ kind: "not_running" });
  });

  it("sweep handles multiple distributing pools in one pass", async () => {
    mock.setupPoolDistribution(POOL_A, 2);
    mock.setupPoolDistribution(POOL_B, 4);
    const pools = [POOL_A, POOL_B];

    const sweep = await runDistributionSweep(mock, pools, 0);

    expect(sweep.madeProgress).toBe(true);
    expect(sweep.pools[POOL_A]?.batches).toBe(2);
    expect(sweep.pools[POOL_B]?.batches).toBe(4);
  });

  it("honors the inner-loop safety cap (maxBatches)", async () => {
    // Pool has 1000 batches queued, but we pass maxBatches=50.
    mock.setupPoolDistribution(POOL_A, 1000);

    const result = await drainPool(mock, POOL_A, 0, /* maxBatches */ 50);

    expect(result.batches).toBe(50);
    expect(result.complete).toBe(false); // we bailed early
    expect(result.madeProgress).toBe(true);
  });

  it("surfaces unexpected errors as `errored` (operator-attention path)", async () => {
    mock.setupPoolDistribution(POOL_A, 3);
    mock.failNextExecute(POOL_A);

    const result = await drainPool(mock, POOL_A, 0);

    expect(result.madeProgress).toBe(false);
    expect(result.batches).toBe(0);
    expect(result.lastOutcome.kind).toBe("errored");
    if (result.lastOutcome.kind === "errored") {
      expect(result.lastOutcome.detail).toContain("forced failure");
    }
  });

  it("end-to-end: full sweep → complete → idempotent re-sweep", async () => {
    mock.setupPoolDistribution(POOL_A, 5);

    // First sweep drains the pool.
    const first = await runDistributionSweep(mock, [POOL_A], 0);
    expect(first.madeProgress).toBe(true);
    expect(first.pools[POOL_A]?.complete).toBe(true);

    // Second sweep: pool is no longer distributing, no-op.
    const second = await runDistributionSweep(mock, [POOL_A], 0);
    expect(second.madeProgress).toBe(false);
    expect(second.pools[POOL_A]?.lastOutcome).toEqual({ kind: "not_running" });
  });
});
