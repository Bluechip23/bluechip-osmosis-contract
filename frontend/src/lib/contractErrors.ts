// Human-readable explanations for the contract's fail-closed rejections.
//
// The commit threshold is OSMO-denominated — a commit's value toward the
// threshold is simply the attached OSMO, with no oracle anywhere. The only
// price read is a fail-closed on-chain TWAP that budgets the ~20 USDC
// pool-creation fee swap at threshold crossing; when it rejects, the whole
// commit reverts rather than mis-budgeting — the user's funds are never at
// risk, but the raw contract error reads like a failure on their end. Map
// the known cases to plain language plus what to do next.

export interface ExplainedError {
    /** Short sentence shown to the user. */
    message: string;
    /** True when simply retrying shortly is the right action. */
    transient: boolean;
}

const RULES: Array<{ match: RegExp; message: string; transient: boolean }> = [
    {
        // Fee-route TWAP failed or the price tripped the sanity ceiling:
        // the crossing can't budget the pool-creation fee swap right now.
        match: /fee-swap TWAP|plausibility ceiling|TWAP price is zero|failed live TWAP probe/i,
        message:
            'The fee-pricing route is temporarily unavailable, so the pool refuses to ' +
            'proceed rather than mis-budget its network fee. Your funds were not moved. ' +
            'Please try again in a few minutes.',
        transient: true,
    },
    {
        // Crossing reserve not yet funded for the fee swap.
        match: /creation-fee reserve is empty/i,
        message:
            'The pool cannot fund its network fee for the crossing yet. Your funds were ' +
            'not moved. A larger commit funds it automatically — or try again after more ' +
            'commits come in.',
        transient: true,
    },
    {
        // Chain fee denom unroutable: operator/governance attention needed.
        match: /neither the native denom|without a fee-swap budget/i,
        message:
            'The network\'s pool-creation fee is temporarily unpayable by this pool. Your ' +
            'funds were not moved. This needs operator attention — please report it.',
        transient: false,
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
