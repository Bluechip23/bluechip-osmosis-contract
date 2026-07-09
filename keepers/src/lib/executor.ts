import type { TxResult } from "./decisions.js";

/**
 * Minimal interface the keeper loops need to interact with the chain.
 * Defined as an interface (not a concrete class) so tests can provide
 * an in-memory mock that simulates contract behavior — distribution
 * batches, retry-notify state, prune counters — without spinning up a
 * real chain.
 *
 * The real implementation wraps a CosmJS SigningCosmWasmClient; see
 * client.ts.
 */
export interface Executor {
  /** Keeper's own address. Used as the tx sender. */
  readonly address: string;

  /**
   * Execute a contract message. Resolves to a TxResult on success.
   * Rejects with an Error on contract error (NothingToRecover,
   * Unauthorized, etc) or RPC failure.
   */
  execute(contract: string, msg: Record<string, unknown>): Promise<TxResult>;

  /** Query the keeper's own native balance. Used to warn on low gas runway. */
  getBalance(denom: string): Promise<bigint>;

  /**
   * Read-only smart query against a contract. Used by the
   * retry-factory-notify keeper to poll each pool's
   * `FactoryNotifyStatus` query before deciding whether to dispatch a
   * RetryFactoryNotify tx. Querying first means we only spend gas on
   * the (rare) pools that actually need a retry rather than blasting
   * every pool every round.
   *
   * `T` is the deserialised JSON shape the contract returns for `msg`.
   * Errors propagate so callers can decide policy (ignore-and-skip vs
   * propagate-and-stop).
   */
  queryContractSmart<T>(contract: string, msg: Record<string, unknown>): Promise<T>;
}
