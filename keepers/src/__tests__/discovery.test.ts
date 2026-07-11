import { describe, expect, it } from "vitest";
import { discoverCommitPools, resolveWatchList } from "../lib/discovery.js";

const FACTORY = "bluechip1factory";

function querierFromPages(pages: Array<Array<{ pool_id: number; pool_addr: string }>>) {
  let call = 0;
  return {
    calls: [] as Array<Record<string, unknown>>,
    async queryContractSmart<T>(_c: string, msg: Record<string, unknown>): Promise<T> {
      (this.calls as Array<Record<string, unknown>>).push(msg);
      const page = pages[Math.min(call, pages.length - 1)] ?? [];
      call += 1;
      return { pools: page } as T;
    },
  };
}

describe("pool discovery", () => {
  it("collects every registered pool and pages with start_after", async () => {
    const fullPage = Array.from({ length: 100 }, (_, i) => ({
      pool_id: i + 1,
      pool_addr: `bluechip1pool_${i + 1}`,
    }));
    const lastPage = [
      { pool_id: 101, pool_addr: "bluechip1pool_101" },
    ];
    const q = querierFromPages([fullPage, lastPage]);

    const pools = await discoverCommitPools(q, FACTORY);
    expect(pools).toHaveLength(101); // 100 pools in page 1 + 1 in page 2
    expect(pools).toContain("bluechip1pool_101");
    expect(pools).toContain("bluechip1pool_2");
    // Page 2 must resume after the last pool_id of page 1.
    expect(q.calls[1]).toEqual({ pools: { start_after: 100, limit: 100 } });
  });

  it("resolveWatchList prefers the static list and survives discovery failure", async () => {
    const failing = {
      async queryContractSmart<T>(): Promise<T> {
        throw new Error("rpc down");
      },
    };
    // Static list wins without touching the chain.
    expect(await resolveWatchList(failing, FACTORY, ["bluechip1pinned"], [])).toEqual([
      "bluechip1pinned",
    ]);
    // Discovery failure keeps the previous good list instead of blanking it.
    expect(await resolveWatchList(failing, FACTORY, [], ["bluechip1last_good"])).toEqual([
      "bluechip1last_good",
    ]);
  });
});
