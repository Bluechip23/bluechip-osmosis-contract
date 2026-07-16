import React, { useState, useEffect } from 'react';
import { Card, CardContent, Typography, Box, LinearProgress } from '@mui/material';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, ReferenceLine } from 'recharts';
import { SigningCosmWasmClient } from '@cosmjs/cosmwasm-stargate';

interface CommitTrackerProps {
    client: SigningCosmWasmClient | null;
    address: string;
    contractAddress: string;
}

interface CommitData {
    last_committed: string;
    total_paid_usd: string;
    total_paid_bluechip: string;
}

interface GraphDataPoint {
    name: string;
    value: number;
    total: number;
    timestamp: string;
}

const CommitTracker: React.FC<CommitTrackerProps> = ({ client, address, contractAddress }) => {
    const [commits, setCommits] = useState<CommitData[]>([]);
    const [totalRaised, setTotalRaised] = useState(0);
    const [totalBluechips, setTotalBluechips] = useState(0);
    const [graphData, setGraphData] = useState<GraphDataPoint[]>([]);
    const [loading, setLoading] = useState(false);
    // USD threshold, read live from the pool (factory-configured;
    // $25,000 is the default). Falls back to the default until loaded.
    const [threshold, setThreshold] = useState(25000);

    useEffect(() => {
        if (client && contractAddress) {
            fetchCommits();
        }
    }, [client, contractAddress]);

    const fetchCommits = async () => {
        if (!client) return;

        setLoading(true);
        try {
            // The commit target is USD-denominated (6 decimals) and set by
            // factory config — read it from the pool rather than hardcoding.
            try {
                const status = await client.queryContractSmart(contractAddress, {
                    is_fully_commited: {}
                });
                if (status && typeof status === 'object' && 'in_progress' in status) {
                    setThreshold(parseInt(status.in_progress.target) / 1_000_000);
                }
            } catch (err) {
                console.error('Error fetching commit target:', err);
            }

            const response = await client.queryContractSmart(contractAddress, {
                pool_commits: {
                    pool_contract_address: contractAddress,
                    limit: 100
                }
            });

            if (response && response.committers) {
                const sortedCommits = [...response.committers].sort((a, b) => {
                    return parseInt(a.last_committed) - parseInt(b.last_committed);
                });

                let cumulative = 0;
                let bluechipTotal = 0;
                const data = sortedCommits.map((commit) => {
                    const value = parseInt(commit.total_paid_usd);
                    const bluechipValue = parseInt(commit.total_paid_bluechip);
                    cumulative += value;
                    bluechipTotal += bluechipValue;

                    return {
                        name: ``,
                        value: value,
                        total: cumulative,
                        timestamp: new Date(parseInt(commit.last_committed) / 1000000).toLocaleString()
                    };
                });

                setCommits(sortedCommits);
                setTotalRaised(cumulative);
                setTotalBluechips(bluechipTotal);
                setGraphData(data);
            }
        } catch (err) {
            console.error("Error fetching commits:", err);
        } finally {
            setLoading(false);
        }
    };

    const displayTotal = totalRaised > 1000000 ? totalRaised / 1000000 : totalRaised;
    const progress = Math.min((displayTotal / threshold) * 100, 100);

    return (
        <Card sx={{ mb: 2 }}>
            <CardContent>
                <Typography variant="h6" gutterBottom>Subscription Tracker</Typography>

                <Box sx={{ mb: 3 }}>
                    <Box sx={{ display: 'flex', justifyContent: 'space-between', mb: 1 }}>
                        <Typography variant="body2">Raised: ${displayTotal.toLocaleString()}</Typography>
                        <Typography variant="body2">Goal: ${threshold.toLocaleString()}</Typography>
                    </Box>
                    <LinearProgress variant="determinate" value={progress} sx={{ height: 10, borderRadius: 5 }} />
                    <Box sx={{ display: 'flex', justifyContent: 'space-between', mt: 0.5 }}>
                        <Typography variant="caption" color="textSecondary">
                            OSMO Committed: {totalBluechips.toLocaleString()}
                        </Typography>
                    </Box>
                </Box>

                <Box sx={{ height: 300, width: '100%' }}>
                    <ResponsiveContainer width="100%" height="100%">
                        <LineChart data={graphData} margin={{ top: 5, right: 20, bottom: 20, left: 20 }}>
                            <CartesianGrid stroke="#ccc" strokeDasharray="5 5" />
                            <XAxis dataKey="name" label={{ value: `Users Committed: ${commits.length}`, offset: -10 }} />
                            <YAxis
                                domain={[0, Math.max(threshold, displayTotal * 1.1)]}
                                label={{ value: 'Subscription Amount', angle: -90, position: 'left', dy: -60, offset: -10 }}
                                tick={{ fontSize: 10 }}
                            />
                            <Tooltip
                                contentStyle={{ backgroundColor: '#333', border: 'none', color: '#fff' }}
                                labelStyle={{ color: '#aaa' }}
                                formatter={(value, name) => [`$${value}`, name === 'total' ? 'Cumulative Total' : 'Transaction Value']}
                            />
                            <ReferenceLine y={threshold} label="Goal" stroke="red" strokeDasharray="3 3" />
                            <Line type="monotone" dataKey="total" stroke="#8884d8" strokeWidth={2} dot={false} activeDot={{ r: 8 }} />
                        </LineChart>
                    </ResponsiveContainer>
                </Box>
            </CardContent>
        </Card>
    );
};

export default CommitTracker;
