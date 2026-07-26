// Human-readable explanations for the contract's fail-closed rejections.
//
// Commits are valued in USD against the pool's threshold using the Pyth
// OSMO/USD price, which the factory reads on-chain behind staleness,
// minimum-age and confidence gates. When a gate rejects the price the whole
// commit reverts rather than pricing the deposit wrong — the user's funds are
// never at risk, but the raw contract error ("Pyth price is stale: age 400s
// exceeds max 300s") reads like a failure on their end. Map the known cases to
// plain language plus what to do next.

export interface ExplainedError {
    /** Short sentence shown to the user. */
    message: string;
    /** True when simply retrying shortly is the right action. */
    transient: boolean;
}

const RULES: Array<{ match: RegExp; message: string; transient: boolean }> = [
    {
        // Staleness gate: the price keeper has lapsed or the feed stopped updating.
        match: /price is stale|stale:? age|exceeds max \d+s/i,
        message:
            'Pricing is temporarily unavailable — the on-chain OSMO/USD price is stale, ' +
            'so the pool refuses to value your commit rather than risk mispricing it. ' +
            'Your funds were not moved. Please try again in a few minutes.',
        transient: true,
    },
    {
        // Minimum-age gate: price was just pushed; needs to age before it can be used.
        match: /too fresh/i,
        message:
            'The price feed was just updated and needs a moment to settle before it can ' +
            'be used. Your funds were not moved. Please try again in about 15 seconds.',
        transient: true,
    },
    {
        // Confidence gate: market too dispersed to trust right now.
        match: /confidence interval too wide/i,
        message:
            'Pricing is paused because the market price is currently too volatile to quote ' +
            'confidently. Your funds were not moved. Please try again shortly.',
        transient: true,
    },
    {
        // Plausibility band / sanity ceiling: misconfiguration, not user-fixable.
        match: /plausibility (ceiling|floor)|InvalidOraclePrice/i,
        message:
            'Pricing failed a safety check and the commit was rejected. Your funds were ' +
            'not moved. This needs operator attention — please report it.',
        transient: false,
    },
    {
        // Oracle unreachable / not configured.
        match: /Pyth pricing is not configured|live Pyth probe|missing price data|no feed id/i,
        message:
            'The price oracle is unreachable, so the pool cannot value your commit. Your ' +
            'funds were not moved. Please try again later or report this if it persists.',
        transient: true,
    },
    {
        // Minimum commit size.
        match: /Commit too small/i,
        message:
            'Your commit is below this pool’s minimum. Increase the amount and try again.',
        transient: false,
    },
    {
        // Per-wallet rate limit on commits/swaps.
        match: /rate limit|too soon|RateLimited/i,
        message:
            'You just interacted with this pool — please wait a few seconds and try again.',
        transient: true,
    },
    {
        // Belief price required post-threshold (slippage protection).
        match: /belief_price is required|BeliefPriceRequired/i,
        message:
            'This pool is live for trading, so a price limit is required to protect you ' +
            'from slippage. Refresh the quote and try again.',
        transient: false,
    },
    {
        // Slippage / min-out not met.
        match: /max spread|token_out_min|slippage|Spread limit exceeded/i,
        message:
            'The price moved more than your slippage limit allowed, so the swap was ' +
            'cancelled and your funds were returned. Refresh the quote and try again.',
        transient: false,
    },
    {
        // Circuit breaker / pause.
        match: /paused|low liquidity/i,
        message:
            'This pool is paused right now, so commits and swaps are temporarily disabled. ' +
            'Your funds were not moved.',
        transient: false,
    },
];

/**
 * Translate a raw contract/tx error into a user-facing explanation.
 * Falls back to the original message when nothing matches.
 */
export function explainContractError(err: unknown): ExplainedError {
    const raw = err instanceof Error ? err.message : String(err ?? '');
    for (const rule of RULES) {
        if (rule.match.test(raw)) {
            return { message: rule.message, transient: rule.transient };
        }
    }
    return { message: raw, transient: false };
}
