import React, { useState } from 'react';
import { Button, Typography, Box } from '@mui/material';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';
import AccountBalanceWalletIcon from '@mui/icons-material/AccountBalanceWallet';
import { OfflineSigner } from '@cosmjs/proto-signing';
import { Coin, GasPrice } from '@cosmjs/stargate';

interface WalletConnectProps {
    setClient: (client: SigningCosmWasmClient | null) => void;
    setAddress: (address: string) => void;
    setBalance: (balance: Coin) => void;
}

// The contracts live on Osmosis, which Keplr ships with out of the box —
// no experimentalSuggestChain needed. Just enable the chain id and connect.
interface NetworkConfig {
    chainId: string;
    chainName: string;
    rpc: string;
    denom: string;
    gasPrice: string;
}

const OSMOSIS_MAINNET: NetworkConfig = {
    chainId: 'osmosis-1',
    chainName: 'Osmosis',
    rpc: 'https://rpc.osmosis.zone',
    denom: 'uosmo',
    gasPrice: '0.025uosmo',
};

const OSMOSIS_TESTNET: NetworkConfig = {
    chainId: 'osmo-test-5',
    chainName: 'Osmosis Testnet',
    rpc: 'https://rpc.osmotest5.osmosis.zone',
    denom: 'uosmo',
    gasPrice: '0.04uosmo',
};

declare global {
    interface Window {
        keplr?: {
            enable: (chainId: string) => Promise<void>;
        };
        getOfflineSigner?: (chainId: string) => OfflineSigner;
    }
}

const WalletConnect: React.FC<WalletConnectProps> = ({ setClient, setAddress, setBalance }) => {
    const [walletAddress, setWalletAddress] = useState<string>('');
    const [error, setError] = useState<string>('');

    const connectToChain = async (config: NetworkConfig): Promise<void> => {
        setError('');

        if (!window.getOfflineSigner || !window.keplr) {
            setError('Please install Keplr extension');
            return;
        }

        try {
            await window.keplr.enable(config.chainId);

            const offlineSigner = window.getOfflineSigner(config.chainId);
            const accounts = await offlineSigner.getAccounts();
            const address = accounts[0].address;

            setWalletAddress(address);
            setAddress(address);

            const client = await SigningCosmWasmClient.connectWithSigner(
                config.rpc,
                offlineSigner,
                { gasPrice: GasPrice.fromString(config.gasPrice) }
            );
            setClient(client);

            const balance = await client.getBalance(address, config.denom);
            setBalance(balance);

        } catch (err) {
            console.error(err);
            const message = err instanceof Error ? err.message : 'Unknown error';
            setError(`Failed to connect: ${message}`);
        }
    };

    return (
        <Box sx={{ mb: 2 }}>
            {walletAddress ? (
                <Typography variant="h6" color="primary">
                    Connected: {walletAddress}
                </Typography>
            ) : (
                <Box sx={{ display: 'flex', gap: 2 }}>
                    <Button
                        variant="contained"
                        startIcon={<AccountBalanceWalletIcon />}
                        onClick={() => connectToChain(OSMOSIS_MAINNET)}
                    >
                        Connect Osmosis
                    </Button>
                    <Button
                        variant="outlined"
                        startIcon={<AccountBalanceWalletIcon />}
                        onClick={() => connectToChain(OSMOSIS_TESTNET)}
                    >
                        Connect Testnet
                    </Button>
                </Box>
            )}
            {error && <Typography color="error">{error}</Typography>}
        </Box>
    );
};

export default WalletConnect;
