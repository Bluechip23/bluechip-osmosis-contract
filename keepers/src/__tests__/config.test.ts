import { describe, expect, it } from "vitest";
import { parseConfig } from "../lib/config.js";

// A minimum env that satisfies every required field. Individual tests
// override or omit specific keys.
const BASE_ENV: Record<string, string> = {
  RPC_ENDPOINT: "http://localhost:26657",
  CHAIN_ID: "bluechip-testnet-1",
  BECH32_PREFIX: "bluechip",
  FACTORY_ADDRESS: "bluechip1factory...",
  KEEPER_MNEMONIC:
    "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima",
};

describe("parseConfig", () => {
  it("accepts a minimum valid environment", () => {
    const cfg = parseConfig(BASE_ENV);
    expect(cfg.RPC_ENDPOINT).toBe("http://localhost:26657");
    expect(cfg.GAS_PRICE).toBe("0.025ubluechip"); // default
    expect(cfg.DISTRIBUTION_POLL_INTERVAL_MS).toBe(1_800_000); // default
    expect(cfg.PRUNE_EVERY_N_SWEEPS).toBe(48); // default
    expect(cfg.PRUNE_BATCH_SIZE).toBe(100); // default
  });

  it("rejects missing required fields", () => {
    const missingRpc = { ...BASE_ENV };
    delete (missingRpc as Record<string, string | undefined>).RPC_ENDPOINT;
    expect(() => parseConfig(missingRpc)).toThrow(/RPC_ENDPOINT/);
  });

  it("rejects empty strings on required fields", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, CHAIN_ID: "" }),
    ).toThrow();
  });

  it("parses POOL_ADDRESSES as a comma-separated list", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      POOL_ADDRESSES: "bluechip1aa, bluechip1bb ,bluechip1cc",
    });
    expect(cfg.POOL_ADDRESSES).toEqual([
      "bluechip1aa",
      "bluechip1bb",
      "bluechip1cc",
    ]);
  });

  it("treats missing POOL_ADDRESSES as empty list (auto-discovery deploy)", () => {
    const cfg = parseConfig(BASE_ENV);
    expect(cfg.POOL_ADDRESSES).toEqual([]);
  });

  it("filters empty entries in POOL_ADDRESSES", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      POOL_ADDRESSES: "bluechip1aa,,   ,bluechip1bb",
    });
    expect(cfg.POOL_ADDRESSES).toEqual(["bluechip1aa", "bluechip1bb"]);
  });

  it("coerces numeric intervals", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      DISTRIBUTION_POLL_INTERVAL_MS: "120000",
      DISTRIBUTION_PER_POOL_DELAY_MS: "500",
    });
    expect(cfg.DISTRIBUTION_POLL_INTERVAL_MS).toBe(120_000);
    expect(cfg.DISTRIBUTION_PER_POOL_DELAY_MS).toBe(500);
  });

  it("coerces MIN_KEEPER_BALANCE_UBLUECHIP to bigint", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      MIN_KEEPER_BALANCE_UBLUECHIP: "5000000",
    });
    expect(cfg.MIN_KEEPER_BALANCE_UBLUECHIP).toBe(5_000_000n);
  });

  it("rejects negative interval values", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, DISTRIBUTION_POLL_INTERVAL_MS: "-1" }),
    ).toThrow(/integer/);
  });

  it("rejects non-numeric interval values", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, DISTRIBUTION_POLL_INTERVAL_MS: "soon" }),
    ).toThrow(/integer/);
  });

  it("rejects zero DISTRIBUTION_POLL_INTERVAL_MS (would busy-loop)", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, DISTRIBUTION_POLL_INTERVAL_MS: "0" }),
    ).toThrow(/integer/);
  });

  it("allows zero DISTRIBUTION_PER_POOL_DELAY_MS (means no delay)", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      DISTRIBUTION_PER_POOL_DELAY_MS: "0",
    });
    expect(cfg.DISTRIBUTION_PER_POOL_DELAY_MS).toBe(0);
  });

  it("allows zero PRUNE_EVERY_N_SWEEPS (disables the prune sweep)", () => {
    const cfg = parseConfig({
      ...BASE_ENV,
      PRUNE_EVERY_N_SWEEPS: "0",
    });
    expect(cfg.PRUNE_EVERY_N_SWEEPS).toBe(0);
  });

  it("rejects zero PRUNE_BATCH_SIZE", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, PRUNE_BATCH_SIZE: "0" }),
    ).toThrow(/integer/);
  });

  it("rejects negative MIN_KEEPER_BALANCE_UBLUECHIP", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, MIN_KEEPER_BALANCE_UBLUECHIP: "-1" }),
    ).toThrow(/non-negative/);
  });

  it("rejects fractional MIN_KEEPER_BALANCE_UBLUECHIP", () => {
    expect(() =>
      parseConfig({ ...BASE_ENV, MIN_KEEPER_BALANCE_UBLUECHIP: "1.5" }),
    ).toThrow(/non-negative/);
  });
});
