import { z } from "zod";

// ---------------------------------------------------------------------------
// Schema — keeper configuration loaded from environment variables
// ---------------------------------------------------------------------------

const nonEmptyString = z.string().min(1);

const commaSeparatedAddresses = z
  .string()
  .optional()
  .transform((raw) => {
    if (!raw) return [] as string[];
    return raw
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  });

// Parses an env-var string as a positive integer (in ms or other units).
// Rejects NaN, negative, zero, or fractional values up front instead of
// letting them silently turn into busy-loops or NaN sleeps. Caller passes
// an `allowZero` flag for the (rare) case where 0 is meaningful (e.g. a
// per-pool delay of 0 = "no delay").
function positiveIntString(opts: { allowZero?: boolean; default: string }) {
  const min = opts.allowZero ? 0 : 1;
  return z
    .string()
    .default(opts.default)
    .transform((s, ctx) => {
      const n = Number.parseInt(s, 10);
      if (!Number.isFinite(n) || !Number.isInteger(n) || n < min) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `must be an integer >= ${min}, got "${s}"`,
        });
        return z.NEVER;
      }
      return n;
    });
}

function nonNegativeBigIntString(defaultValue: string) {
  return z
    .string()
    .default(defaultValue)
    .transform((s, ctx) => {
      // BigInt() accepts negatives and decimals throw — guard explicitly so
      // a stray "-1" or "1.5" in env doesn't silently become a weird threshold.
      if (!/^\d+$/.test(s)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `must be a non-negative integer literal, got "${s}"`,
        });
        return z.NEVER;
      }
      return BigInt(s);
    });
}

/**
 * Full config schema. Every field here is required at runtime unless
 * marked optional. Unknown env vars are simply ignored.
 */
export const ConfigSchema = z.object({
  // Chain connection
  RPC_ENDPOINT: nonEmptyString,
  CHAIN_ID: nonEmptyString,
  BECH32_PREFIX: nonEmptyString,
  GAS_PRICE: nonEmptyString.default("0.025ubluechip"),
  GAS_DENOM: nonEmptyString.default("ubluechip"),

  // Contracts
  FACTORY_ADDRESS: nonEmptyString,
  POOL_ADDRESSES: commaSeparatedAddresses,

  // Wallet
  KEEPER_MNEMONIC: nonEmptyString,

  // Distribution keeper tuning
  DISTRIBUTION_POLL_INTERVAL_MS: positiveIntString({ default: "1800000" }), // 30 min
  // 0 means "no breather"; default is a 2s pause between pool calls so we
  // don't hammer the RPC.
  DISTRIBUTION_PER_POOL_DELAY_MS: positiveIntString({
    allowZero: true,
    default: "2000",
  }),

  // Rate-limit prune sweep cadence (folded into the distribution keeper).
  // Once every N distribution sweeps the keeper also dispatches
  // factory.PruneRateLimits {}. Default 48 × 30min ≈ once a day, which
  // is plenty for the expected rate-limit drift. Set to 0 to disable
  // entirely (e.g., for testnets where rate-limit growth doesn't matter
  // or for ops who'd rather run prune as a separate cron).
  PRUNE_EVERY_N_SWEEPS: positiveIntString({ allowZero: true, default: "48" }),
  // Per-call work cap passed to PruneRateLimits. Contract enforces a
  // hard ceiling of 500; we default to 100 which is plenty for the
  // expected drift rate (≪ 100 stale entries per day on a healthy
  // protocol). Tunable upward for backlog catch-up after a long
  // prune outage.
  PRUNE_BATCH_SIZE: positiveIntString({ default: "100" }),

  // Safety: warn when the keeper wallet's gas runway dips below this.
  // Keeper calls are bounty-less, so the wallet only ever drains — top
  // it up when the warning fires.
  MIN_KEEPER_BALANCE_UBLUECHIP: nonNegativeBigIntString("1000000"),
});

export type Config = z.infer<typeof ConfigSchema>;

/**
 * Parse config from a raw env-like object. Throws a clear ZodError if
 * validation fails.
 */
export function parseConfig(raw: Record<string, string | undefined>): Config {
  return ConfigSchema.parse(raw);
}

/**
 * Convenience: parse the live process.env. Used by the keeper entrypoints.
 */
export function loadConfigFromEnv(): Config {
  return parseConfig(process.env);
}
