# Protocol Multisig — setup and signing (solo-operator edition)

The factory admin key controls config timelocks, pool upgrades, pauses,
and emergency withdrawals; the contract admin (set at instantiate)
controls migrations; `PROTOCOL_WALLET` receives all protocol revenue.
Today all three default to the deploy key — one leaked mnemonic away
from losing the protocol. A multisig fixes that **even for a single
operator**: with a 2-of-3 made of three keys *you* control stored in
different places, an attacker needs two of your devices, and you can
lose any one key and still recover.

Two options; the native route is recommended to start.

---

## Option A (recommended): native Cosmos 2-of-3 multisig

A native multisig is just an address derived from N public keys and a
threshold. No contract, no extra trust — every Cosmos chain supports
it out of the box, and the resulting `osmo1...` address works
everywhere an address is expected: `factory_admin_address`,
`PROTOCOL_WALLET`, and `set-contract-admin`.

### 1. Create three keys, stored in three places

```bash
# Key 1 — your workstation keyring
osmosisd keys add bc-1

# Key 2 — hardware wallet (Ledger with the Cosmos app)
osmosisd keys add bc-2 --ledger

# Key 3 — a second machine, old laptop, or offline VM.
# Generate it THERE, write the mnemonic to paper/steel, then import
# only the PUBLIC key on your workstation:
#   (on the offline machine)  osmosisd keys add bc-3
#   (copy the pubkey shown)   osmosisd keys show bc-3 --pubkey
#   (on your workstation)     osmosisd keys add bc-3 --pubkey '<pubkey JSON>'
```

The point is failure independence: a stolen laptop, a phished mnemonic,
or a dead Ledger each costs you **one** key, and 2-of-3 survives any
single loss.

### 2. Assemble the multisig

```bash
osmosisd keys add bc-admin \
    --multisig bc-1,bc-2,bc-3 \
    --multisig-threshold 2

osmosisd keys show bc-admin -a     # -> osmo1...  THE protocol address
```

Fund it with a little OSMO for gas, then use that address as:

- `PROTOCOL_WALLET` in `osmosis_mainnet.env` (before running
  `./deploy_osmosis.sh`)
- `factory_admin_address` — either at instantiate, or later via the
  48h `ProposeConfigUpdate` flow from the current admin
- contract admin: `osmosisd tx wasm set-contract-admin <contract>
  <multisig-addr> --from <current-admin> ...` for the factory and
  router (the post-deploy checklist step)

> Order of operations if you already deployed with the deploy key:
> first `set-contract-admin` both contracts to the multisig, then
> propose+apply a factory config update moving
> `factory_admin_address` (and `bluechip_wallet_address` if desired)
> to the multisig. From that moment every admin action needs two
> signatures.

### 3. Signing a transaction (the 2-of-3 dance)

Every admin action becomes generate → sign twice → combine → broadcast.
Worked example — proposing a factory config change:

```bash
FACTORY=osmo1...            # factory address
MULTI=$(osmosisd keys show bc-admin -a)
NODE=https://rpc.osmosis.zone:443
CHAIN=osmosis-1

# 3a. Generate the unsigned tx (any machine, no keys needed)
osmosisd tx wasm execute "$FACTORY" "$(cat proposal.json)" \
    --from "$MULTI" --generate-only \
    --node $NODE --chain-id $CHAIN \
    --gas 400000 --gas-prices 0.025uosmo \
    > unsigned.json
# proposal.json = {"propose_config_update":{"config":{ ...full config... }}}

# 3b. Sign with two of the three keys. Multisig member signatures MUST
#     use amino-json sign mode:
osmosisd tx sign unsigned.json --from bc-1 \
    --multisig "$MULTI" --sign-mode amino-json \
    --node $NODE --chain-id $CHAIN > sig-bc1.json

osmosisd tx sign unsigned.json --from bc-2 --ledger \
    --multisig "$MULTI" --sign-mode amino-json \
    --node $NODE --chain-id $CHAIN > sig-bc2.json
# (an offline key signs the same file on its own machine; move the
#  small sig-*.json files around on a USB stick — never the mnemonic)

# 3c. Combine and broadcast (any machine)
osmosisd tx multisign unsigned.json bc-admin sig-bc1.json sig-bc2.json \
    --node $NODE --chain-id $CHAIN > signed.json
osmosisd tx broadcast signed.json --node $NODE
```

48 hours later, repeat the same dance with
`{"update_config":{}}` to apply. **Calendar both steps** — a proposal
that expires unapplied must be re-proposed and re-waited.

Everyday admin payloads, for reference:

| Action | execute msg |
|---|---|
| Propose factory config | `{"propose_config_update":{"config":{...}}}` |
| Apply after 48h | `{"update_config":{}}` |
| Cancel pending | `{"cancel_config_update":{}}` |
| Pause a pool | `{"pause_pool":{"pool_id":N}}` |
| Unpause | `{"unpause_pool":{"pool_id":N}}` |

Tips that save real pain:

- Use a **fixed `--gas` amount** (not `auto`) when generating: gas
  simulation can't run against a multisig without registered pubkey.
- The multisig's pubkey registers on-chain with its **first outbound
  tx**; sending funds *to* it works immediately.
- `--account-number` and `--sequence` can be pinned
  (`osmosisd q auth account $MULTI`) if signing offline/air-gapped.
- Test the entire dance on **osmo-test-5 first** with throwaway keys —
  the flow has sharp edges (sign mode, sequence mismatches) you want
  to hit on testnet, not while responding to an incident.

## Option B: DAODAO multisig (contract-based, web UI)

[DAODAO](https://daodao.zone) deploys a cw4/cw3-style multisig with a
web interface: members, thresholds, proposals, and arbitrary wasm
execute messages, all point-and-click. The DAO's contract address
serves as admin/wallet exactly like a native multisig address.

- **Pros:** far nicer signing UX; add members in minutes when bluechip
  stops being a one-person team; proposal history is public and
  self-documenting.
- **Cons:** your admin key becomes a *contract* — you inherit its code
  risk and upgrade policy on top of your own; and you depend on the UI
  for convenient operation.

A reasonable path: start with the native 2-of-3 (zero extra trust),
move to DAODAO when a second human joins.

## What goes behind the multisig — checklist

- [ ] `set-contract-admin` (migration authority): factory, router
- [ ] `factory_admin_address` (config/pause/upgrade/emergency admin)
- [ ] `bluechip_wallet_address` / `PROTOCOL_WALLET` (revenue) — pools
      live-query the factory for this, so rotating it later is one
      config cycle
- [ ] NOT the keeper wallet — the keeper stays a low-value hot key
      with gas money only
- [ ] Faucet/test keys never reused as multisig members
