import React, { useState, useEffect } from 'react';
import { Card, CardContent, Typography, TextField, Button, Box, Alert } from '@mui/material';
import { coins } from '@cosmjs/stargate';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';

interface SwapProps {
    client: SigningCosmWasmClient | null;
    address: string;
    contractAddress: string;
}

// Both sides of every pool are native bank denoms now — OSMO (uosmo) and
// the creator token's TokenFactory denom (factory/{pool_addr}/{subdenom}).
// A swap in either direction is the same simple_swap message with the
// offered denom attached as funds; the old CW20 send-hook path is gone.
const Swap = ({ client, address, contractAddress }: SwapProps) => {
    const [offerDenom, setOfferDenom] = useState('');
    const [amount, setAmount] = useState('');
    const [maxSpread, setMaxSpread] = useState('0.005'); // Default 0.5%
    const [deadline, setDeadline] = useState('20'); // Default 20 minutes
    const [targetContractAddress, setTargetContractAddress] = useState(contractAddress || '');
    const [status, setStatus] = useState('');

    // Sync with global contract address if it changes
    useEffect(() => {
        if (contractAddress) {
            setTargetContractAddress(contractAddress);
        }
    }, [contractAddress]);

    const handleSwap = async () => {
        if (!client || !address || !targetContractAddress) {
            setStatus('Please connect wallet and set contract address');
            return;
        }

        try {
            setStatus('Swapping...');

            // Convert amount to micro-units
            const amountVal = parseFloat(amount);
            if (isNaN(amountVal) || amountVal <= 0) {
                setStatus('Error: Please enter a valid positive amount');
                return;
            }
            const amountInMicroUnits = Math.floor(amountVal * 1_000_000).toString();

            // Calculate deadline in nanoseconds (optional - use null if not provided)
            const deadlineInNs = deadline && parseFloat(deadline) > 0
                ? (Date.now() + (parseFloat(deadline) * 60 * 1000)) * 1000000
                : null;

            // TokenFactory creator denoms look like factory/{pool}/{sub};
            // anything else (uosmo, ibc/...) is the pool's native side.
            const isCreatorToken = offerDenom.startsWith('factory/');
            const offerAsset = {
                info: isCreatorToken
                    ? { creator_token: { denom: offerDenom } }
                    : { bluechip: { denom: offerDenom } },
                amount: amountInMicroUnits
            };

            // Fix belief_price from a live quote so a front-run that moves
            // the pool reverts the swap instead of filling at a worse price.
            let beliefPrice: string | null = null;
            const sim = await client.queryContractSmart(targetContractAddress, {
                simulation: { offer_asset: offerAsset }
            });
            const expectedOut = Number(sim?.return_amount ?? 0);
            if (Number.isFinite(expectedOut) && expectedOut > 0) {
                // belief_price = offer / expected_out (offer-per-ask).
                beliefPrice = (Number(amountInMicroUnits) / expectedOut).toFixed(18);
            }

            const msg = {
                simple_swap: {
                    offer_asset: offerAsset,
                    belief_price: beliefPrice,
                    max_spread: maxSpread || null,
                    allow_high_max_spread: null,
                    to: null,
                    transaction_deadline: deadlineInNs ? deadlineInNs.toString() : null
                }
            };

            const funds = coins(amountInMicroUnits, offerDenom);

            const result = await client.execute(
                address,
                targetContractAddress,
                msg,
                {
                    amount: [],
                    gas: "500000"
                },
                "Swap",
                funds
            );
            console.log("Transaction Hash:", result.transactionHash);
            setStatus(`Success! Tx Hash: ${result.transactionHash}`);
        } catch (err) {
            console.error(err);
            setStatus('Error: ' + (err as Error).message);
        }
    };

    return (
        <Card sx={{ mb: 2 }}>
            <CardContent>
                <Typography variant="h6" gutterBottom>Swap</Typography>
                <Box sx={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                    <TextField
                        label="Pool Contract Address"
                        value={targetContractAddress}
                        onChange={(e) => setTargetContractAddress(e.target.value)}
                        placeholder="osmo1..."
                        helperText="Address of the creator pool contract to swap with"
                    />
                    <TextField
                        label="Offer Denom"
                        value={offerDenom}
                        onChange={(e) => setOfferDenom(e.target.value)}
                        helperText="uosmo to buy, or the creator token's factory/... denom to sell"
                    />
                    <TextField
                        label="Amount"
                        value={amount}
                        onChange={(e) => setAmount(e.target.value)}
                        type="number"
                    />
                    <TextField
                        label="Max Spread (Decimal)"
                        value={maxSpread}
                        onChange={(e) => setMaxSpread(e.target.value)}
                        helperText="e.g. 0.005 for 0.5%"
                    />
                    <TextField
                        label="Deadline (minutes)"
                        value={deadline}
                        onChange={(e) => setDeadline(e.target.value)}
                        type="number"
                        helperText="Transaction deadline in minutes"
                    />
                    <Button variant="contained" color="secondary" onClick={handleSwap}>
                        Swap
                    </Button>
                    {status && <Alert severity={status.includes('Success') ? 'success' : 'info'}>{status}</Alert>}
                </Box>
            </CardContent>
        </Card>
    );
};

export default Swap;
