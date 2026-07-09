import "dotenv/config";
import { loadConfigFromEnv } from "./lib/config.js";
import { buildKeeperClient } from "./lib/client.js";
import { nextDistributionSleepMs } from "./lib/decisions.js";
import { runDistributionSweep } from "./lib/distribution-loop.js";
import { checkKeeperBalance } from "./lib/balance.js";
import { resolveWatchList } from "./lib/discovery.js";
import { runPruneIteration } from "./lib/prune-loop.js";
import { runRetryNotifySweep } from "./lib/retry-notify-loop.js";
import { interruptibleSleep } from "./lib/sleep.js";
import { log } from "./lib/logger.js";

async function main(): Promise<void> {
  const cfg = loadConfigFromEnv();

  if (cfg.POOL_ADDRESSES.length === 0) {
    log.info(
      "POOL_ADDRESSES is empty — auto-discovering commit pools from the factory registry each sweep",
    );
  }

  log.info("distribution keeper starting", {
    rpc: cfg.RPC_ENDPOINT,
    chain: cfg.CHAIN_ID,
    pools: cfg.POOL_ADDRESSES.length,
    interval_ms: cfg.DISTRIBUTION_POLL_INTERVAL_MS,
  });

  const client = await buildKeeperClient(cfg);
  log.info("keeper wallet ready", { address: client.address });

  let stopped = false;
  const stop = () => {
    stopped = true;
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  // Sweep counter for the rate-limit prune. We dispatch
  // factory.PruneRateLimits once every PRUNE_EVERY_N_SWEEPS distribution
  // sweeps. A counter (rather than wall-clock cadence) keeps the
  // sequence-number tx ordering simple — the prune always runs right
  // after a sweep on the same wallet. PRUNE_EVERY_N_SWEEPS == 0
  // disables the sweep.
  let pruneCounter = 0;
  if (cfg.PRUNE_EVERY_N_SWEEPS === 0) {
    log.info("rate-limit prune sweep disabled (PRUNE_EVERY_N_SWEEPS=0)");
  } else {
    log.info("rate-limit prune sweep enabled", {
      every_n_sweeps: cfg.PRUNE_EVERY_N_SWEEPS,
      batch_size: cfg.PRUNE_BATCH_SIZE,
    });
  }

  let watchList: string[] = cfg.POOL_ADDRESSES;
  while (!stopped) {
    watchList = await resolveWatchList(
      client,
      cfg.FACTORY_ADDRESS,
      cfg.POOL_ADDRESSES,
      watchList,
    );

    // Run the retry-factory-notify sweep BEFORE the distribution sweep.
    // A stuck factory-notify means the factory's POOL_THRESHOLD_CROSSED
    // registry entry never landed. It does NOT block distribution
    // itself, but settling the notify first means the same iteration
    // can leave the pool fully consistent rather than half. Each
    // pool's RetryFactoryNotify is permissionless and idempotent on
    // the factory side (POOL_THRESHOLD_CROSSED gate), so a redundant
    // call is at worst wasted gas.
    await runRetryNotifySweep(client, watchList);

    const { madeProgress } = await runDistributionSweep(
      client,
      watchList,
      cfg.DISTRIBUTION_PER_POOL_DELAY_MS,
    );
    await checkKeeperBalance(client, cfg.GAS_DENOM, cfg.MIN_KEEPER_BALANCE_UBLUECHIP);

    // Once every PRUNE_EVERY_N_SWEEPS sweeps, also run the rate-limit
    // prune. Independent of the sweep's success — a quiet distribution
    // round doesn't mean we shouldn't prune; the two are unrelated
    // chain operations.
    if (cfg.PRUNE_EVERY_N_SWEEPS > 0) {
      pruneCounter += 1;
      if (pruneCounter >= cfg.PRUNE_EVERY_N_SWEEPS) {
        pruneCounter = 0;
        await runPruneIteration(client, cfg.FACTORY_ADDRESS, cfg.PRUNE_BATCH_SIZE);
      }
    }

    const ms = nextDistributionSleepMs(cfg.DISTRIBUTION_POLL_INTERVAL_MS, madeProgress);
    log.info("sleeping", { ms, made_progress: madeProgress });
    await interruptibleSleep(ms, () => stopped);
  }

  log.info("distribution keeper shutting down");
  client.close();
}

main().catch((err) => {
  log.error("distribution keeper crashed", {
    detail: err instanceof Error ? err.message : String(err),
  });
  process.exit(1);
});
