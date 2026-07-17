// types/index.ts

import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';

// Wire format of the contracts' TokenType enum. The `bluechip` key is the
// native side (OSMO on Osmosis — the serde rename is load-bearing on-chain),
// and `creator_token` is the pool's TokenFactory denom
// (`factory/{pool_addr}/{subdenom}`). Both sides are bank denoms — the
// creator side is keyed by `denom`, NOT `contract_addr` (the CW20 model
// was removed in the Osmosis migration).
export type TokenType =
    | { creator_token: { denom: string } }
    | { bluechip: { denom: string } };

export interface TokenInfo {
    name: string;
    symbol: string;
    decimals: number;
    total_supply: string;
}

export interface DiscoverToken {
    // TokenFactory denom of the creator token (factory/{pool}/{sub}).
    tokenDenom: string;
    poolAddress: string;
    name: string;
    symbol: string;
    decimals: number;
    price?: string;
    priceChange24h: number;
    volume24h?: string;
    marketCap?: string;
    thresholdReached: boolean;
}

// Token as displayed in Portfolio page (has balance)
export interface PortfolioToken {
    tokenDenom: string;
    poolAddress: string;
    name: string;
    symbol: string;
    decimals: number;
    balance: string;
    thresholdReached: boolean;
}

// Union type for modals that can accept either
export type ModalToken = DiscoverToken | PortfolioToken;

// Type guard to check if token has balance
export const hasBalance = (token: ModalToken): token is PortfolioToken => {
    return 'balance' in token && token.balance !== undefined;
};

export interface PoolDetails {
    asset_infos: [TokenType, TokenType];
    contract_addr: string;
    pool_type: { xyk: Record<string, never> } | { stable: Record<string, never> };
}

// Response from pool contract's `pool_state` query. Post-migration the
// reserves are read live from the native Osmosis GAMM pool (zero until the
// threshold crossing seeds it). `nft_ownership_accepted` and
// `total_liquidity` are retained for wire compatibility only.
export interface PoolStateResponse {
    nft_ownership_accepted: boolean;
    reserve0: string;
    reserve1: string;
    total_liquidity: string;
    block_time_last: number;
}

// On-chain CommitStatus enum: unit variant serializes as string "fully_committed",
// struct variant serializes as { in_progress: { raised, target } }
export type CommitStatus =
    | 'fully_committed'
    | { in_progress: { raised: string; target: string } };

export const isThresholdReached = (status: CommitStatus): boolean => {
    return status === 'fully_committed';
};

// Response from the pool's `creator_earnings` query.
export interface CreatorEarningsResponse {
    creator_wallet_address: string;
    excess: {
        bluechip_amount: string;
        token_amount: string;
        unlock_time: string;
        claimable_now: boolean;
    } | null;
    is_threshold_hit: boolean;
    threshold_crossed_at: string | null;
}

// Response from the pool's `distribution_state` query (null when no
// distribution is active).
export interface DistributionStateResponse {
    is_distributing: boolean;
    distributions_remaining: number;
    last_processed_key: string | null;
    started_at: string;
    last_updated: string;
    seconds_since_update: number;
    is_stalled: boolean;
    consecutive_failures: number;
    total_to_distribute: string;
    total_committed_usd: string;
    distributed_so_far: string;
}

// ============================================
// Modal Props Types
// ============================================

export interface BaseModalProps {
    open: boolean;
    onClose: () => void;
    client: SigningCosmWasmClient | null;
    address: string;
}

export interface TokenModalProps extends BaseModalProps {
    token: ModalToken;
}

export interface InfoModalProps {
    open: boolean;
    onClose: () => void;
    token: ModalToken;
}

// ============================================
// Transaction Types
// ============================================

export interface TransactionResult {
    success: boolean;
    txHash?: string;
    error?: string;
}

// ============================================
// Wallet Types
// ============================================

export interface WalletState {
    client: SigningCosmWasmClient | null;
    address: string;
    balance: {
        amount: string;
        denom: string;
    } | null;
    connected: boolean;
}

// ============================================
// Config Types
// ============================================

export interface ChainConfig {
    chainId: string;
    chainName: string;
    rpc: string;
    rest: string;
    factoryAddress: string;
    nativeDenom: string;
    coinDecimals: number;
}

// Default config — the osmo-test-5 testnet, where the contracts are
// currently deployed (osmo_testnet_v2). Override the factory address and
// endpoints via VITE_* env vars (e.g. for the future mainnet deployment).
// `nativeDenom` is only a fallback — the live denom is read from the
// factory config / pool `pair {}` query wherever a real pool is involved.
export const DEFAULT_CHAIN_CONFIG: ChainConfig = {
    chainId: 'osmo-test-5',
    chainName: 'Osmosis Testnet',
    rpc: 'https://rpc.osmotest5.osmosis.zone',
    rest: 'https://lcd.osmotest5.osmosis.zone',
    factoryAddress: import.meta.env.VITE_FACTORY_ADDRESS
        || 'osmo1p93hcfzjnjfv0vtfxmunpqc25tq3p2vzh76hq3wxfz2zyayw4hzq4ac3vt',
    nativeDenom: 'uosmo',
    coinDecimals: 6,
};

// ============================================
// Utility Functions
// ============================================

export const formatTokenAmount = (amount: string, decimals: number): string => {
    const num = parseInt(amount) / Math.pow(10, decimals);
    return num.toLocaleString(undefined, { maximumFractionDigits: decimals });
};

export const toMicroUnits = (amount: string, decimals: number): string => {
    const num = parseFloat(amount);
    if (isNaN(num)) return '0';
    return Math.floor(num * Math.pow(10, decimals)).toString();
};

export const fromMicroUnits = (amount: string, decimals: number): number => {
    return parseInt(amount) / Math.pow(10, decimals);
};

// Extract the creator token's TokenFactory denom from pool asset_infos.
// (Renamed from getCreatorTokenAddress — the creator token is a TokenFactory
// denom now, not a CW20 address.)
export const getCreatorTokenDenom = (assetInfos: [TokenType, TokenType]): string | null => {
    const creatorToken = assetInfos.find(
        (asset): asset is { creator_token: { denom: string } } =>
            'creator_token' in asset
    );
    return creatorToken?.creator_token.denom ?? null;
};

// Extract the native (OSMO) denom from pool asset_infos
export const getBluechipDenom = (assetInfos: [TokenType, TokenType]): string | null => {
    const bluechip = assetInfos.find(
        (asset): asset is { bluechip: { denom: string } } =>
            'bluechip' in asset
    );
    return bluechip?.bluechip.denom ?? null;
};

// Derive a display symbol from a TokenFactory denom
// (factory/{pool_addr}/{subdenom} -> SUBDENOM).
export const symbolFromDenom = (denom: string): string => {
    const parts = denom.split('/');
    return (parts.length === 3 ? parts[2] : denom).toUpperCase();
};
