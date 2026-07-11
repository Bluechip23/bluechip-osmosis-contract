import { log } from "./logger.js";

// Pool auto-discovery via the factory's paginated `pools` registry
// query. Replaces the hand-maintained POOL_ADDRESSES list: a keeper
// pointed at the factory automatically picks up every commit pool,
// including ones created after the keeper started. Every registered
// pool is a commit pool, so the full registry is the watch list.

interface PoolListEntry {
  pool_id: number;
  pool_addr: string;
}

interface PoolsResponse {
  pools: PoolListEntry[];
}

interface SmartQuerier {
  queryContractSmart<T>(contract: string, msg: Record<string, unknown>): Promise<T>;
}

const PAGE_LIMIT = 100;
const MAX_PAGES = 50;

export async function discoverCommitPools(
  client: SmartQuerier,
  factoryAddress: string,
): Promise<string[]> {
  const found: string[] = [];
  let startAfter: number | null = null;
  for (let page = 0; page < MAX_PAGES; page++) {
    const res: PoolsResponse = await client.queryContractSmart<PoolsResponse>(factoryAddress, {
      pools: { start_after: startAfter, limit: PAGE_LIMIT },
    });
    for (const p of res.pools) {
      found.push(p.pool_addr);
    }
    if (res.pools.length < PAGE_LIMIT) break;
    startAfter = res.pools[res.pools.length - 1]!.pool_id;
  }
  return found;
}

// Resolve the watch list for one sweep: an explicit POOL_ADDRESSES env
// list wins (lets operators pin a subset); otherwise discover from the
// factory registry. Discovery failures fall back to the last good list
// so a flaky RPC round doesn't blank out the watch set mid-flight.
export async function resolveWatchList(
  client: SmartQuerier,
  factoryAddress: string,
  staticList: string[],
  lastGood: string[],
): Promise<string[]> {
  if (staticList.length > 0) return staticList;
  try {
    const discovered = await discoverCommitPools(client, factoryAddress);
    if (discovered.length !== lastGood.length) {
      log.info("pool discovery updated watch list", {
        pools: discovered.length,
        previously: lastGood.length,
      });
    }
    return discovered;
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    log.warn("pool discovery failed — keeping previous watch list", {
      detail,
      pools: lastGood.length,
    });
    return lastGood;
  }
}
