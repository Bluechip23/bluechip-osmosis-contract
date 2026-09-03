// Explicit StdFee construction for CosmWasm execute calls. Osmosis
// enforces a non-zero base fee (EIP-1559-style fee market), so a
// `{ amount: [], gas }` fee is rejected by mainnet nodes — price the gas
// limit at the average Osmosis gas price (matches the GAMM signing
// client's default in osmosisGamm.ts).

export const GAS_PRICE_UOSMO_PER_GAS = 0.025;

export function stdFee(gasLimit: number | string, denom = 'uosmo') {
    const gas = Number(gasLimit);
    return {
        amount: [{ denom, amount: Math.ceil(gas * GAS_PRICE_UOSMO_PER_GAS).toString() }],
        gas: gas.toString(),
    };
}
