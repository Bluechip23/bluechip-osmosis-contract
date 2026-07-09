import { describe, expect, it } from "vitest";
import {
  classifyTx,
  isDistributionComplete,
  nextDistributionSleepMs,
  readWasmAttribute,
  shouldContinueSamePool,
  type TxResult,
} from "../lib/decisions.js";

// ---------------------------------------------------------------------------
// Tx fixture helpers
// ---------------------------------------------------------------------------

function okTxWithWasmAttrs(attrs: Array<[string, string]>): TxResult {
  return {
    code: 0,
    transactionHash: "DEADBEEF",
    events: [
      {
        type: "wasm",
        attributes: attrs.map(([key, value]) => ({ key, value })),
      },
    ],
  };
}

function failedTx(rawLog: string): TxResult {
  return {
    code: 5,
    transactionHash: "FAILED",
    rawLog,
  };
}

// ---------------------------------------------------------------------------
// readWasmAttribute
// ---------------------------------------------------------------------------

describe("readWasmAttribute", () => {
  it("returns the value for a matching wasm event attribute", () => {
    const tx = okTxWithWasmAttrs([["distribution_complete", "true"]]);
    expect(readWasmAttribute(tx, "distribution_complete")).toBe("true");
  });

  it("returns undefined when the key isn't present", () => {
    const tx = okTxWithWasmAttrs([["action", "continue_distribution"]]);
    expect(readWasmAttribute(tx, "distribution_complete")).toBeUndefined();
  });

  it("returns undefined when events array is absent", () => {
    const tx: TxResult = { code: 0, transactionHash: "X" };
    expect(readWasmAttribute(tx, "anything")).toBeUndefined();
  });

  it("ignores non-wasm event types", () => {
    const tx: TxResult = {
      code: 0,
      transactionHash: "X",
      events: [
        {
          type: "transfer",
          attributes: [{ key: "distribution_complete", value: "nope" }],
        },
      ],
    };
    expect(readWasmAttribute(tx, "distribution_complete")).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// classifyTx
// ---------------------------------------------------------------------------

describe("classifyTx", () => {
  it("classifies a successful tx as ok", () => {
    const tx = okTxWithWasmAttrs([["action", "continue_distribution"]]);
    expect(classifyTx(tx)).toEqual({ kind: "ok" });
  });

  it("classifies a failed tx", () => {
    const tx = failedTx("out of gas");
    expect(classifyTx(tx)).toEqual({ kind: "failed", rawLog: "out of gas" });
  });

  it("defaults rawLog when the failed tx carries none", () => {
    const tx: TxResult = { code: 11, transactionHash: "X" };
    expect(classifyTx(tx)).toEqual({ kind: "failed", rawLog: "tx failed" });
  });
});

// ---------------------------------------------------------------------------
// Sleep heuristics
// ---------------------------------------------------------------------------

describe("nextDistributionSleepMs", () => {
  it("polls quickly after making progress", () => {
    const sleep = nextDistributionSleepMs(1_800_000, true, 15_000);
    expect(sleep).toBe(15_000);
  });

  it("polls at full interval when idle", () => {
    const sleep = nextDistributionSleepMs(1_800_000, false, 15_000);
    expect(sleep).toBe(1_800_000);
  });

  it("clamps fast-poll to base if base is smaller", () => {
    const sleep = nextDistributionSleepMs(5_000, true, 15_000);
    expect(sleep).toBe(5_000);
  });
});

// ---------------------------------------------------------------------------
// isDistributionComplete
// ---------------------------------------------------------------------------

describe("isDistributionComplete", () => {
  it("returns true when attribute is 'true'", () => {
    const tx = okTxWithWasmAttrs([["distribution_complete", "true"]]);
    expect(isDistributionComplete(tx)).toBe(true);
  });

  it("returns false when attribute is 'false'", () => {
    const tx = okTxWithWasmAttrs([["distribution_complete", "false"]]);
    expect(isDistributionComplete(tx)).toBe(false);
  });

  it("returns false when attribute is absent", () => {
    const tx = okTxWithWasmAttrs([["action", "continue_distribution"]]);
    expect(isDistributionComplete(tx)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// shouldContinueSamePool
// ---------------------------------------------------------------------------

describe("shouldContinueSamePool", () => {
  it("stops when distribution is complete even on an ok outcome", () => {
    expect(shouldContinueSamePool({ kind: "ok" }, true)).toBe(false);
  });

  it("continues on ok + incomplete", () => {
    expect(shouldContinueSamePool({ kind: "ok" }, false)).toBe(true);
  });

  it("stops on failed outcomes", () => {
    expect(
      shouldContinueSamePool({ kind: "failed", rawLog: "x" }, false),
    ).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// isExpectedSkipError — classify contract errors as routine skips vs. real errors
// ---------------------------------------------------------------------------

import { isExpectedSkipError } from "../lib/types.js";

describe("isExpectedSkipError", () => {
  it("treats NothingToRecover as a skip", () => {
    expect(isExpectedSkipError("NothingToRecover: distribution not in progress")).toBe(true);
  });

  it("treats the pool's no-pending-notify message as a skip", () => {
    expect(
      isExpectedSkipError(
        "execute wasm contract failed: No pending factory notification to retry",
      ),
    ).toBe(true);
  });

  it("treats the factory's threshold-already-recorded idempotency error as a skip", () => {
    // Exact #[error(...)] display string from the factory's
    // NotifyThresholdCrossed idempotency gate — the display form is what
    // reaches the client over RPC, not the Rust variant name.
    expect(
      isExpectedSkipError(
        "failed to execute message; message index: 0: Threshold crossing " +
          "already recorded for this pool: execute wasm contract failed",
      ),
    ).toBe(true);
  });

  it("does not treat arbitrary runtime errors as skips", () => {
    expect(isExpectedSkipError("account does not exist")).toBe(false);
    expect(isExpectedSkipError("insufficient funds")).toBe(false);
  });
});
