import type { Executor } from "./executor.js";
import { log } from "./logger.js";

/**
 * Best-effort keeper-wallet balance check. Never throws — a failed
 * balance query returns a warning but doesn't break the loop.
 *
 * Keeper calls no longer earn bounties, so the wallet only ever drains
 * on gas; this warning is the operator's cue to top up.
 */
export async function checkKeeperBalance(
  executor: Executor,
  denom: string,
  minBalance: bigint,
): Promise<void> {
  try {
    const balance = await executor.getBalance(denom);
    if (balance < minBalance) {
      log.warn("keeper balance below threshold — top up soon", {
        address: executor.address,
        balance: balance.toString(),
        threshold: minBalance.toString(),
      });
    }
  } catch (err) {
    log.warn("balance check failed", {
      detail: err instanceof Error ? err.message : String(err),
    });
  }
}
