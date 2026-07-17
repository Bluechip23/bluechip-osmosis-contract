import { useState } from 'react';
import { Card, CardContent, Typography, TextField, Button, Box, Alert, IconButton, Tooltip } from '@mui/material';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';
import { DEFAULT_CHAIN_CONFIG } from '../types/FrontendTypes';

// Factory contract address - configured during deployment.
const FACTORY_ADDRESS =
    import.meta.env.VITE_FACTORY_ADDRESS ||
    'cosmos1yvgh8xeju5dyr0zxlkvq09htvhjj20fncp5g58np4u25g8rkpgjst8ghg8';

interface CreatePoolProps {
    client: SigningCosmWasmClient | null;
    address: string;
}

const CreatePool = ({ client, address }: CreatePoolProps) => {
    const [tokenName, setTokenName] = useState('');
    const [tokenSymbol, setTokenSymbol] = useState('');
    const [status, setStatus] = useState('');
    const [txHash, setTxHash] = useState('');
    const [copySuccess, setCopySuccess] = useState(false);

    const handleCreatePool = async () => {
        if (!client || !address) {
            setStatus('Please connect your wallet');
            return;
        }

        try {
            setStatus('Creating pool...');
            setTxHash('');
            setCopySuccess(false);

            // The crossing is funded from commits, but CREATE still mints the
            // TokenFactory denom (MsgCreateDenom consumes ~1M gas on Osmosis,
            // DenomCreationGasConsume) + registers metadata + runs the reply
            // chain, so budget generously above a plain execute.
            const gas = '3000000';

            if (!tokenName || !tokenSymbol) {
                setStatus('Error: Creator pools require a token name and symbol');
                return;
            }

            // The bluechip side MUST equal the factory's canonical
            // `bluechip_denom` (uosmo on Osmosis) or `validate_pool_token_info`
            // rejects the create. Read it live from factory config instead of
            // trusting a hard-coded default so this works on any deployment.
            const factoryCfg = await client.queryContractSmart(FACTORY_ADDRESS, { factory: {} });
            const bluechipDenom: string =
                factoryCfg?.factory?.bluechip_denom ?? DEFAULT_CHAIN_CONFIG.nativeDenom;

            // Create { pool_msg, token_info }. Only `pool_token_info` and the
            // token metadata are caller-supplied; commit threshold, fee splits,
            // threshold-payout amounts, lock caps, and oracle config come from
            // factory config. The creator-token slot is a PLACEHOLDER — the
            // pool creates its own `factory/{pool}/{sub}` TokenFactory denom at
            // instantiate — but it must still be a syntactically valid
            // CreatorToken, i.e. keyed by `denom` (NOT `contract_addr`; the
            // CW20 model was removed in the Osmosis migration).
            const createMsg: Record<string, unknown> = {
                create: {
                    pool_msg: {
                        pool_token_info: [
                            { bluechip: { denom: bluechipDenom } },
                            { creator_token: { denom: 'WILL_BE_CREATED_BY_FACTORY' } },
                        ],
                    },
                    token_info: {
                        name: tokenName,
                        symbol: tokenSymbol,
                        // Pool enforces 6 decimals to match the fixed payout amounts.
                        decimal: 6,
                    },
                },
            };

            console.log('Creating pool with message:', JSON.stringify(createMsg, null, 2));

            const result = await client.execute(
                address,
                FACTORY_ADDRESS,
                createMsg,
                { amount: [], gas },
                'Create',
            );

            console.log('Transaction Hash:', result.transactionHash);
            setTxHash(result.transactionHash);
            setStatus('Success! Pool creation transaction submitted.');

            setTokenName('');
            setTokenSymbol('');
        } catch (err) {
            console.error('Full error:', err);
            setStatus('Error: ' + (err as Error).message);
            setTxHash('');
        }
    };

    const handleCopyTxHash = () => {
        navigator.clipboard.writeText(txHash);
        setCopySuccess(true);
        setTimeout(() => setCopySuccess(false), 2000);
    };

    return (
        <Card sx={{ mb: 2 }}>
            <CardContent>
                <Typography variant="h6" gutterBottom>Create Pool</Typography>
                <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
                    Creator pools start in commit phase and mint a fresh CW20.
                </Typography>

                <Box sx={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                    <TextField
                        label="Token Name"
                        value={tokenName}
                        onChange={(e) => setTokenName(e.target.value)}
                        placeholder="My Creator Token"
                        helperText="Full name of the new CW20 the factory will mint"
                        required
                    />
                    <TextField
                        label="Token Symbol (Ticker)"
                        value={tokenSymbol}
                        onChange={(e) => setTokenSymbol(e.target.value.toUpperCase())}
                        placeholder="MCT"
                        helperText="Short ticker symbol (e.g. BTC, ETH)"
                        required
                        inputProps={{ maxLength: 10 }}
                    />
                    <Box sx={{ p: 2, bgcolor: 'info.light', borderRadius: 1 }}>
                        <Typography variant="subtitle2" sx={{ fontWeight: 'bold', mb: 1 }}>
                            Factory-Configured (read at call time)
                        </Typography>
                        <Typography variant="body2">- Commit threshold (USD)</Typography>
                        <Typography variant="body2">- Commit fee splits (bluechip / creator)</Typography>
                        <Typography variant="body2">- Threshold-payout amounts (creator / bluechip / pool seed / committers)</Typography>
                        <Typography variant="body2">- Max bluechip lock per pool & creator excess lock days</Typography>
                        <Typography variant="body2">- Pyth oracle address & price feed id</Typography>
                        <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 1 }}>
                            The frontend no longer forwards these — the factory consults its own stored config. Per-address create cooldown: 1h.
                        </Typography>
                    </Box>

                    <Button variant="contained"
                        color="primary"
                        onClick={handleCreatePool}
                        disabled={!client || !address || !tokenName || !tokenSymbol}
                    >
                        Create Creator Pool
                    </Button>

                    {status && (
                        <Alert severity={status.includes('Success') ? 'success' : status.includes('Error') ? 'error' : 'info'}>
                            {status}
                        </Alert>
                    )}

                    {txHash && (
                        <Box sx={{
                            p: 2,
                            bgcolor: 'success.light',
                            borderRadius: 1,
                            border: '1px solid',
                            borderColor: 'success.main',
                        }}>
                            <Typography variant="subtitle2" sx={{ mb: 1, fontWeight: 'bold' }}>
                                Transaction Hash:
                            </Typography>
                            <Box sx={{ display: 'flex', alignItems: 'center', gap: 1 }}>
                                <Typography
                                    variant="body2"
                                    sx={{
                                        fontFamily: 'monospace',
                                        wordBreak: 'break-all',
                                        flex: 1,
                                        fontSize: '0.85rem',
                                    }}
                                >
                                    {txHash}
                                </Typography>
                                <Tooltip title={copySuccess ? 'Copied!' : 'Copy to clipboard'}>
                                    <IconButton
                                        size="small"
                                        onClick={handleCopyTxHash}
                                        color={copySuccess ? 'success' : 'primary'}
                                    >
                                        <ContentCopyIcon fontSize="small" />
                                    </IconButton>
                                </Tooltip>
                            </Box>
                        </Box>
                    )}
                </Box>
            </CardContent>
        </Card>
    );
};

export default CreatePool;
