import type { Executor } from "../lib/executor.js";
import type { TxEvent, TxResult } from "../lib/decisions.js";

/**
 * In-memory simulation of the on-chain factory + pools. Implements
 * the Executor interface so the real keeper loops can run against it
 * in tests. Models the contract-side invariants we care about:
 *
 *   - Pools support ContinueDistribution. A pool not in distribution
 *     state throws a NothingToRecover-style error.
 *   - Pools with distribution state emit distribution_complete=false
 *     until drained, then true on the final batch. No bounty is paid
 *     — distribution batches are pure gas cost for the keeper.
 *   - Pools support RetryFactoryNotify + the FactoryNotifyStatus query,
 *     mirroring the pending-notify recovery flow.
 *   - The factory supports the permissionless PruneRateLimits sweep.
 */

export interface MockContractsOptions {
  /** Factory contract address. */
  factoryAddress: string;
}

interface PoolState {
  isDistributing: boolean;
  batchesRemaining: number;
  /**
   * Mirrors the on-chain `PENDING_FACTORY_NOTIFY` flag. True when the
   * pool's threshold-cross commit landed but the factory-side
   * NotifyThresholdCrossed SubMsg failed. RetryFactoryNotify clears
   * this on success. Tests set it explicitly to drive the retry-notify
   * keeper.
   */
  pendingFactoryNotify?: boolean;
  /**
   * If true, RetryFactoryNotify dispatches against this pool throw
   * synthetically — e.g., simulating a still-failing factory side
   * (idempotency error). Used to exercise the "tx_skip" expected-error
   * branch of the retry-notify keeper.
   */
  retryFails?: boolean;
}

let txCounter = 0;

function nextHash(): string {
  txCounter++;
  return `TX${txCounter.toString().padStart(8, "0")}`;
}

function wasmEvent(attrs: Array<[string, string]>): TxEvent {
  return {
    type: "wasm",
    attributes: attrs.map(([key, value]) => ({ key, value })),
  };
}

export class MockContracts implements Executor {
  readonly address: string;
  private readonly factoryAddress: string;
  private keeperBalance: bigint;
  private pools = new Map<string, PoolState>();
  // Observability: every execute() call is recorded here so tests can
  // assert exactly which messages the keeper dispatched (and which it
  // correctly avoided dispatching).
  public readonly calls: Array<{ contract: string; msg: Record<string, unknown> }> = [];
  // Test hook: make the next execute() against a given address throw.
  // Used to simulate a transient RPC/contract failure.
  private failOnceAddresses = new Set<string>();

  constructor(address: string, opts: MockContractsOptions) {
    this.address = address;
    this.factoryAddress = opts.factoryAddress;
    this.keeperBalance = 1_000_000_000n;
  }

  /** Test hook: set a pool's distribution state. */
  setupPoolDistribution(address: string, batches: number): void {
    const existing = this.pools.get(address);
    this.pools.set(address, {
      ...existing,
      isDistributing: true,
      batchesRemaining: batches,
    });
  }

  /** Test hook: set the keeper wallet's balance. */
  setKeeperBalance(amount: bigint): void {
    this.keeperBalance = amount;
  }

  /** Test hook: arm a pool's PENDING_FACTORY_NOTIFY flag. */
  setPendingFactoryNotify(address: string, pending: boolean): void {
    const pool = this.pools.get(address) ?? {
      isDistributing: false,
      batchesRemaining: 0,
    };
    pool.pendingFactoryNotify = pending;
    this.pools.set(address, pool);
  }

  /** Test hook: future RetryFactoryNotify on this pool throws. */
  failNextRetryNotify(address: string, fail: boolean = true): void {
    const pool = this.pools.get(address) ?? {
      isDistributing: false,
      batchesRemaining: 0,
    };
    pool.retryFails = fail;
    this.pools.set(address, pool);
  }

  /**
   * Test hook: track every PruneRateLimits call dispatched against the
   * factory so tests can assert cadence and batch_size threading.
   */
  public readonly pruneCalls: Array<{ batchSize: number }> = [];
  /**
   * Test hook: programmable counters returned by the next prune call.
   * Defaults to (0, 0) — i.e., a steady-state "nothing to prune" sweep.
   */
  private nextPruneCounters: { commit: number; standard: number } = {
    commit: 0,
    standard: 0,
  };
  setNextPruneCounters(commit: number, standard: number): void {
    this.nextPruneCounters = { commit, standard };
  }

  /** Test hook: the next execute() against `address` throws. One-shot. */
  failNextExecute(address: string): void {
    this.failOnceAddresses.add(address);
  }

  /**
   * Test hook: the next queryContractSmart() against `address` throws
   * with `error`. One-shot. Used to simulate transient RPC blips on
   * the retry-notify keeper's pre-flight FactoryNotifyStatus query so
   * we can assert it logs query_failed and never dispatches a tx.
   */
  private failOnceQueryAddresses = new Map<string, string>();
  failNextQuery(address: string, error: string = "RPC: connection reset"): void {
    this.failOnceQueryAddresses.set(address, error);
  }

  // Executor impl --------------------------------------------------------

  async execute(contract: string, msg: Record<string, unknown>): Promise<TxResult> {
    this.calls.push({ contract, msg });
    if (this.failOnceAddresses.has(contract)) {
      this.failOnceAddresses.delete(contract);
      throw new Error(`mock: forced failure on ${contract}`);
    }
    if (contract === this.factoryAddress) {
      return this.executeFactory(msg);
    }
    // Otherwise treat as pool.
    return this.executePool(contract, msg);
  }

  async getBalance(_denom: string): Promise<bigint> {
    return this.keeperBalance;
  }

  async queryContractSmart<T>(contract: string, msg: Record<string, unknown>): Promise<T> {
    const failError = this.failOnceQueryAddresses.get(contract);
    if (failError !== undefined) {
      this.failOnceQueryAddresses.delete(contract);
      throw new Error(failError);
    }
    if ("factory_notify_status" in msg) {
      // Mirror creator-pool::query::query_factory_notify_status —
      // returns { pending: bool } reading from the pool's
      // PENDING_FACTORY_NOTIFY storage. We model "no pool entry" as
      // pending=false, matching the contract's `unwrap_or(false)`.
      const pool = this.pools.get(contract);
      return { pending: pool?.pendingFactoryNotify ?? false } as unknown as T;
    }
    throw new Error(`mock query unsupported: ${JSON.stringify(msg)}`);
  }

  // Factory handlers -----------------------------------------------------

  private executeFactory(msg: Record<string, unknown>): TxResult {
    if ("prune_rate_limits" in msg) {
      return this.executePruneRateLimits(msg);
    }
    throw new Error(`factory: unknown message ${JSON.stringify(msg)}`);
  }

  private executePruneRateLimits(msg: Record<string, unknown>): TxResult {
    // Capture the batch_size so tests can assert it threaded through
    // from PRUNE_BATCH_SIZE config.
    const inner = (msg as { prune_rate_limits: { batch_size?: number } })
      .prune_rate_limits;
    const batchSize = inner?.batch_size ?? 100;
    this.pruneCalls.push({ batchSize });

    const counters = this.nextPruneCounters;
    // Reset for next call to the steady-state default; tests opt back
    // in via setNextPruneCounters.
    this.nextPruneCounters = { commit: 0, standard: 0 };

    return {
      code: 0,
      transactionHash: nextHash(),
      events: [
        wasmEvent([
          ["action", "prune_rate_limits"],
          ["commit_pruned", counters.commit.toString()],
          ["standard_pruned", counters.standard.toString()],
          ["stale_after_secs", "36000"],
          ["batch_size", batchSize.toString()],
        ]),
      ],
    };
  }

  // Pool handlers --------------------------------------------------------

  private executePool(poolAddress: string, msg: Record<string, unknown>): TxResult {
    if ("continue_distribution" in msg) {
      return this.executeContinueDistribution(poolAddress);
    }
    if ("retry_factory_notify" in msg) {
      return this.executeRetryFactoryNotify(poolAddress);
    }
    throw new Error(`pool: unknown message ${JSON.stringify(msg)}`);
  }

  /**
   * Mirror creator-pool::contract::execute_retry_factory_notify.
   *
   * Three behaviors:
   *   - pool has no pending notify → throw the canonical
   *     "No pending factory notification to retry" error (treated as
   *     an expected skip by the keeper).
   *   - pool's `retryFails` flag is set → throw the factory's
   *     idempotency-gate error ("Threshold crossing already recorded
   *     for this pool") so we exercise the keeper's tx_skip recovery
   *     branch.
   *   - happy path → clear pendingFactoryNotify, emit attributes.
   */
  private executeRetryFactoryNotify(poolAddress: string): TxResult {
    const pool = this.pools.get(poolAddress);
    if (!pool || !pool.pendingFactoryNotify) {
      throw new Error("No pending factory notification to retry");
    }
    if (pool.retryFails) {
      pool.retryFails = false;
      throw new Error("Threshold crossing already recorded for this pool");
    }
    pool.pendingFactoryNotify = false;
    return {
      code: 0,
      transactionHash: nextHash(),
      events: [
        wasmEvent([
          ["action", "retry_factory_notify"],
          ["pool_id", "1"],
        ]),
      ],
    };
  }

  private executeContinueDistribution(poolAddress: string): TxResult {
    const pool = this.pools.get(poolAddress);
    if (!pool || !pool.isDistributing) {
      // Mirrors NothingToRecover / storage-not-found errors.
      throw new Error("NothingToRecover: distribution not in progress");
    }

    pool.batchesRemaining = Math.max(0, pool.batchesRemaining - 1);
    const complete = pool.batchesRemaining === 0;
    if (complete) {
      pool.isDistributing = false;
    }

    return {
      code: 0,
      transactionHash: nextHash(),
      events: [
        wasmEvent([
          ["action", "continue_distribution"],
          ["distribution_complete", complete ? "true" : "false"],
        ]),
      ],
    };
  }
}
