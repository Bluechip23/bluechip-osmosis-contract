# BlueChip Frontend Integration Guide

> **This guide is for website owners, content creators, and community builders** who want to add BlueChip buttons and features to their own website. You do **not** need to be a programmer — just copy and paste the code blocks below.

> **Chain note:** BlueChip runs on **Osmosis**. Payments are made in **OSMO** (`uosmo`), addresses look like `osmo1...`, and creator tokens are native Osmosis **TokenFactory** denoms that look like `factory/osmo1poolAddress/utoken` — they are ordinary bank coins, not token contracts. Once a pool crosses its funding threshold, its liquidity lives in a **native Osmosis GAMM pool**, so creator tokens can also be traded directly on Osmosis.

---

## Table of Contents

1. [Prerequisites — What You Need First](#1-prerequisites--what-you-need-first)
2. [Quick Start — Add the Script Tags](#2-quick-start)
3. [Connecting to Keplr Wallet](#3-connecting-to-keplr-wallet)
4. [Subscribe Button (Commit)](#4-subscribe-button-commit)
5. [Buy Button (Swap OSMO for Creator Tokens)](#5-buy-button-swap-osmo-for-creator-tokens)
6. [Sell Button (Swap Creator Tokens for OSMO)](#6-sell-button-swap-creator-tokens-for-osmo)
7. [Cross-Token Swaps (Router)](#7-cross-token-swaps-router)
8. [Liquidity — It's a Native Osmosis Pool](#8-liquidity--its-a-native-osmosis-pool)
9. [Create a Pool](#9-create-a-pool)
10. [Querying Pool Info (Read-Only)](#10-querying-pool-info-read-only)
11. [Granting Special Privileges to Committed Users](#11-granting-special-privileges-to-committed-users)
12. [Full Working Example Page](#12-full-working-example-page)
13. [Troubleshooting](#13-troubleshooting)
14. [Contract Address Reference](#14-contract-address-reference)

---

## 1. Prerequisites — What You Need First

### For Your Visitors (People Using Your Website)

Your visitors will need the **Keplr Wallet** browser extension to interact with BlueChip buttons on your site. Keplr supports Osmosis out of the box — no custom chain registration required.

**Install Keplr:**
- **Chrome / Brave / Edge:** [Install from Chrome Web Store](https://chrome.google.com/webstore/detail/keplr/dmkamcknogkgcdfhhbddcghachkejeap)
- **Firefox:** [Install from Firefox Add-ons](https://addons.mozilla.org/en-US/firefox/addon/keplr/)
- **Mobile:** [Keplr Mobile App (iOS)](https://apps.apple.com/us/app/keplr-wallet/id1567851089) | [Keplr Mobile App (Android)](https://play.google.com/store/apps/details?id=com.chainapsis.keplr)
- **Official Website:** [https://www.keplr.app/get](https://www.keplr.app/get)

> **Tip:** If a visitor does not have Keplr installed, the code below will show them a friendly message with a link to install it.

### For You (The Website Owner)

You need:
1. A website where you can add HTML and JavaScript (WordPress, Squarespace with code injection, a custom site, etc.)
2. Your **Pool Contract Address** — this is the address of your creator pool on Osmosis (looks like `osmo1abc...xyz`)
3. Your **Factory Contract Address** — only needed if you want to create new pools

---

## 2. Quick Start

### Fastest path: the BlueChip widget (recommended)

If all you want is a **Subscribe button** and/or **subscriber-gated
content**, skip this whole document and use the prebuilt widget — one
script tag, and the only thing you edit is your pool address:

```html
<script src="https://cdn.jsdelivr.net/gh/Bluechip23/bluechipblockexplorer@main/widget/dist/bluechip-widget.min.js"></script>

<div data-bluechip-subscribe data-pool="osmo1YOUR_POOL_ADDRESS" data-amount="25"></div>

<div data-bluechip-gate data-pool="osmo1YOUR_POOL_ADDRESS" data-min-usd="5">
    Subscriber-only content.
</div>
```

Full options (custom labels, fixed amounts, JS API):
[widget/README.md](https://github.com/Bluechip23/bluechipblockexplorer/tree/main/widget)
in the bluechipblockexplorer repo.

### Manual path: load CosmJS yourself

The rest of this guide's code blocks talk to the chain through CosmJS.
**CosmJS does not publish a ready-made browser bundle** (there is no
`build/bundle.js` on unpkg — a plain script tag will 404), so pick one:

- **Sites with a bundler (React, Vite, Next, etc.):**
  `npm install @cosmjs/cosmwasm-stargate@0.32.4` and import it; adapt the
  snippets from `window.CosmWasmClient.X` to your imports.
- **Plain HTML sites:** load it as an ES module from a
  CommonJS-to-ESM CDN and expose the global the snippets expect:

```html
<script type="module">
    import * as cosmwasm from "https://esm.sh/@cosmjs/cosmwasm-stargate@0.32.4";
    window.CosmWasmClient = cosmwasm;   // snippets use CosmWasmClient.SigningCosmWasmClient
    window.dispatchEvent(new Event("cosmjs-ready"));
</script>
```

(Any button handler that runs before the module finishes loading will see
`CosmWasmClient` undefined — either wait for the `cosmjs-ready` event or
just let users click again. The prebuilt widget above has none of these
caveats, which is why it's the recommended path.)

Then add this configuration block. **Replace the placeholder addresses** with your actual addresses:

```html
<script>
// ============================================================
//  BLUECHIP CONFIGURATION — EDIT THESE VALUES
// ============================================================
const BLUECHIP_CONFIG = {
    // Chain settings — Osmosis mainnet. (Testnet: chainId "osmo-test-5",
    // rpc https://rpc.osmotest5.osmosis.zone, rest https://lcd.osmotest5.osmosis.zone)
    chainId:        "osmosis-1",
    chainName:      "Osmosis",
    rpc:            "https://rpc.osmosis.zone",
    rest:           "https://lcd.osmosis.zone",
    nativeDenom:    "uosmo",
    coinDecimals:   6,

    // Your contract addresses — REPLACE THESE
    factoryAddress: "osmo1factory_address_here",        // Factory contract
    poolAddress:    "osmo1your_pool_address_here",      // Your creator pool
    routerAddress:  "osmo1router_address_here",         // Multi-hop router (Section 7)
};
</script>
```

> Keplr ships with Osmosis built in, so there is **no**
> `experimentalSuggestChain` step and no bech32/currency config to copy —
> just `enable("osmosis-1")` and go.

---

## 3. Connecting to Keplr Wallet

Every BlueChip interaction starts by connecting the user's Keplr wallet. Add this script **once** on any page where you have BlueChip buttons:

```html
<script>
// ============================================================
//  WALLET CONNECTION
//  Stores: window.bluechipClient, window.bluechipAddress
// ============================================================

// Global wallet state
window.bluechipClient  = null;
window.bluechipAddress = "";

async function connectKeplrWallet() {
    // ---- Check if Keplr is installed ----
    if (!window.keplr || !window.getOfflineSigner) {
        // Show a friendly install message
        var msg = document.getElementById("bluechip-wallet-status");
        if (msg) {
            msg.innerHTML =
                '<div style="padding:12px;background:#fff3cd;border:1px solid #ffc107;border-radius:6px;">' +
                '<strong>Keplr Wallet Required</strong><br>' +
                'Please install the Keplr browser extension to continue.<br><br>' +
                '<a href="https://www.keplr.app/get" target="_blank" ' +
                'style="color:#0d6efd;font-weight:bold;">Click here to install Keplr &rarr;</a>' +
                '</div>';
        }
        alert(
            "Keplr wallet not detected!\n\n" +
            "Install it from: https://www.keplr.app/get"
        );
        return false;
    }

    try {
        // Osmosis ships with Keplr — enable it directly (no
        // experimentalSuggestChain needed).
        await window.keplr.enable(BLUECHIP_CONFIG.chainId);

        // Get signer and address
        var offlineSigner = window.getOfflineSigner(BLUECHIP_CONFIG.chainId);
        var accounts      = await offlineSigner.getAccounts();
        var address       = accounts[0].address;

        // Connect the signing client
        var client = await CosmWasmClient.SigningCosmWasmClient.connectWithSigner(
            BLUECHIP_CONFIG.rpc,
            offlineSigner
        );

        // Store globally
        window.bluechipClient  = client;
        window.bluechipAddress = address;

        // Update UI
        var statusEl = document.getElementById("bluechip-wallet-status");
        if (statusEl) {
            statusEl.innerHTML =
                '<div style="padding:8px 12px;background:#d4edda;border:1px solid #28a745;' +
                'border-radius:6px;font-family:monospace;word-break:break-all;">' +
                'Connected: ' + address + '</div>';
        }

        // Fetch balance
        var balance = await client.getBalance(address, BLUECHIP_CONFIG.nativeDenom);
        var balanceEl = document.getElementById("bluechip-balance");
        if (balanceEl) {
            var human = (parseInt(balance.amount) / Math.pow(10, BLUECHIP_CONFIG.coinDecimals)).toFixed(6);
            balanceEl.textContent = human + " OSMO";
        }

        return true;
    } catch (err) {
        console.error("Wallet connection failed:", err);
        var statusEl = document.getElementById("bluechip-wallet-status");
        if (statusEl) {
            statusEl.innerHTML =
                '<div style="padding:8px 12px;background:#f8d7da;border:1px solid #dc3545;' +
                'border-radius:6px;">Connection failed: ' + err.message + '</div>';
        }
        return false;
    }
}
</script>
```

**Add a Connect Wallet button to your page:**

```html
<!-- CONNECT WALLET BUTTON — Copy this wherever you want it -->
<div style="margin:16px 0;">
    <button onclick="connectKeplrWallet()"
            style="padding:12px 24px;font-size:16px;font-weight:bold;
                   background:#4CAF50;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Connect Keplr Wallet
    </button>
    <div id="bluechip-wallet-status" style="margin-top:8px;"></div>
    <div id="bluechip-balance" style="margin-top:4px;font-weight:bold;"></div>
</div>
```

---

## 4. Subscribe Button (Commit)

The **Subscribe** button lets your fans commit OSMO to your creator pool. This is how people support you. Before the pool reaches its USD threshold ($25,000 by default), commits are recorded in a ledger. After the threshold is crossed, commits are swapped through the native Osmosis pool and your supporter receives your creator tokens.

**A 6% fee is deducted:** 1% goes to the BlueChip protocol, 5% goes to you the creator.

> **Post-threshold commits require a `belief_price`.** Once the pool is
> active, a commit is a market buy — and the pool **rejects**
> `belief_price: null` on that path (it is the anti-sandwich floor; there
> is no `minimum_receive` backstop like the router has). The handler below
> takes a live `simulation` quote at submit time and derives
> `belief_price = offer / expected_out`. Pre-threshold commits don't swap,
> so they leave it `null`.

```html
<!-- ============================================================ -->
<!--  SUBSCRIBE BUTTON                                            -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #4CAF50;
            border-radius:12px;background:#f9fff9;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#2e7d32;">Subscribe (Commit)</h3>
    <p style="color:#666;font-size:14px;">
        Support this creator by committing OSMO.
        6% fee: 1% protocol + 5% creator.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount (OSMO):
    </label>
    <input id="subscribe-amount" type="number" placeholder="e.g. 100"
           style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Max Spread (optional):
    </label>
    <input id="subscribe-spread" type="text" value="0.005" placeholder="0.005 = 0.5%"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleSubscribe()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#4CAF50;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Subscribe
    </button>

    <div id="subscribe-status" style="margin-top:12px;"></div>
    <div id="subscribe-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleSubscribe() {
    var statusEl = document.getElementById("subscribe-status");
    var txEl     = document.getElementById("subscribe-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    // Ensure wallet is connected
    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var amount = parseFloat(document.getElementById("subscribe-amount").value);
    if (isNaN(amount) || amount <= 0) {
        statusEl.innerHTML = '<div style="color:red;">Please enter a valid amount.</div>';
        return;
    }

    var spreadInput = document.getElementById("subscribe-spread").value;

    statusEl.innerHTML = '<div style="color:#1565c0;">Subscribing...</div>';

    try {
        // Convert to micro-units (1 OSMO = 1,000,000 uosmo)
        var microAmount = Math.floor(amount * 1000000).toString();

        // Check pool threshold status
        var thresholdStatus = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { is_fully_commited: {} }
        );
        var isThresholdCrossed = (thresholdStatus === "fully_committed");

        // Post-threshold, the commit swaps through the native pool and the
        // contract REQUIRES an explicit belief_price. Derive it from a live
        // quote: if the price moves against the user before the tx lands,
        // the swap reverts instead of filling at the worse price.
        var beliefPrice = null;
        if (isThresholdCrossed) {
            var sim = await window.bluechipClient.queryContractSmart(
                BLUECHIP_CONFIG.poolAddress,
                { simulation: { offer_asset: {
                    info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
                    amount: microAmount
                } } }
            );
            var expectedOut = parseInt(sim.return_amount);
            if (!expectedOut || expectedOut <= 0) {
                statusEl.innerHTML = '<div style="color:red;">Could not quote this commit — try again.</div>';
                return;
            }
            // belief_price = offer / expected_out (offer-per-ask).
            beliefPrice = (parseInt(microAmount) / expectedOut).toFixed(18);
        }

        // Deadline: 20 minutes from now, in nanoseconds
        var deadlineNs = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

        // Build the commit message
        var msg = {
            commit: {
                asset: {
                    info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
                    amount: microAmount
                },
                transaction_deadline: deadlineNs,
                belief_price:         beliefPrice,
                max_spread:           (isThresholdCrossed && spreadInput) ? spreadInput : null
            }
        };

        // Attach the OSMO as funds
        var funds = [{ denom: BLUECHIP_CONFIG.nativeDenom, amount: microAmount }];

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,
            msg,
            { amount: [], gas: "600000" },
            "Commit",
            funds
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Success!</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#e8f5e9;border:1px solid #4CAF50;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#4CAF50;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Subscribe error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 5. Buy Button (Swap OSMO for Creator Tokens)

The **Buy** button lets people swap their OSMO for your creator tokens. This only works **after** the pool has crossed the USD threshold and its native Osmosis pool exists. (Since it's a normal Osmosis pool, buyers can also just trade it on [app.osmosis.zone](https://app.osmosis.zone) — the contract's `simple_swap` is a convenience venue with the same result.)

```html
<!-- ============================================================ -->
<!--  BUY BUTTON — Swap OSMO → Creator Tokens                     -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #1976d2;
            border-radius:12px;background:#f3f8ff;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#1565c0;">Buy Creator Tokens</h3>
    <p style="color:#666;font-size:14px;">
        Swap your OSMO for this creator's tokens.
        Only available after the pool threshold is reached.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount (OSMO to spend):
    </label>
    <input id="buy-amount" type="number" placeholder="e.g. 50"
           style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Max Spread:
    </label>
    <input id="buy-spread" type="text" value="0.005" placeholder="0.005 = 0.5%"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleBuy()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#1976d2;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Buy Tokens
    </button>

    <div id="buy-status" style="margin-top:12px;"></div>
    <div id="buy-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleBuy() {
    var statusEl = document.getElementById("buy-status");
    var txEl     = document.getElementById("buy-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var amount = parseFloat(document.getElementById("buy-amount").value);
    if (isNaN(amount) || amount <= 0) {
        statusEl.innerHTML = '<div style="color:red;">Please enter a valid amount.</div>';
        return;
    }

    var spreadInput = document.getElementById("buy-spread").value;
    statusEl.innerHTML = '<div style="color:#1565c0;">Processing swap...</div>';

    try {
        var microAmount = Math.floor(amount * 1000000).toString();
        var deadlineNs  = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

        var offerAsset = {
            info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
            amount: microAmount
        };

        // Take a live quote and fix belief_price from it. simple_swap
        // accepts belief_price: null, but setting it is what actually
        // bounds sandwiching — a front-run that moves the pool makes the
        // swap revert instead of filling at the worse price.
        var beliefPrice = null;
        var sim = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { simulation: { offer_asset: offerAsset } }
        );
        var expectedOut = parseInt(sim.return_amount);
        if (expectedOut > 0) {
            beliefPrice = (parseInt(microAmount) / expectedOut).toFixed(18);
        }

        // SimpleSwap: send OSMO, receive the creator's TokenFactory tokens
        var msg = {
            simple_swap: {
                offer_asset:           offerAsset,
                belief_price:          beliefPrice,
                max_spread:            spreadInput || null,
                // Set to true to bypass the pool's spread safety cap. Leave
                // null in the standard buy flow; only flip on if the user
                // has explicitly opted into a higher max_spread than the cap.
                allow_high_max_spread: null,
                to:                    null,
                transaction_deadline:  deadlineNs
            }
        };

        var funds = [{ denom: BLUECHIP_CONFIG.nativeDenom, amount: microAmount }];

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,
            msg,
            { amount: [], gas: "500000" },
            "Buy Token",
            funds
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Success! Tokens purchased.</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#e3f2fd;border:1px solid #1976d2;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#1976d2;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Buy error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 6. Sell Button (Swap Creator Tokens for OSMO)

The **Sell** button lets people swap their creator tokens back into OSMO. Creator tokens are **native TokenFactory coins**, so a sell is the exact same `simple_swap` message as a buy — just with the creator token's denom attached as funds instead of OSMO. There is **no CW20 `send` step and no token contract address** anymore.

> **You need the creator token's denom** (looks like `factory/osmo1pool.../utoken`), which you can read from the pool's `pair` query (see [Section 10](#10-querying-pool-info-read-only)).

```html
<!-- ============================================================ -->
<!--  SELL BUTTON — Swap Creator Tokens → OSMO                    -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #d32f2f;
            border-radius:12px;background:#fff5f5;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#c62828;">Sell Creator Tokens</h3>
    <p style="color:#666;font-size:14px;">
        Swap creator tokens back to OSMO.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Creator Token Denom:
    </label>
    <input id="sell-token-denom" type="text" placeholder="factory/osmo1pool.../utoken"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount (Creator Tokens):
    </label>
    <input id="sell-amount" type="number" placeholder="e.g. 1000"
           style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Max Spread:
    </label>
    <input id="sell-spread" type="text" value="0.005" placeholder="0.005 = 0.5%"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleSell()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#d32f2f;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Sell Tokens
    </button>

    <div id="sell-status" style="margin-top:12px;"></div>
    <div id="sell-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleSell() {
    var statusEl = document.getElementById("sell-status");
    var txEl     = document.getElementById("sell-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var tokenDenom  = document.getElementById("sell-token-denom").value.trim();
    var amount      = parseFloat(document.getElementById("sell-amount").value);
    var spreadInput = document.getElementById("sell-spread").value;

    if (!tokenDenom) {
        statusEl.innerHTML = '<div style="color:red;">Please enter the creator token denom (factory/...).</div>';
        return;
    }
    if (isNaN(amount) || amount <= 0) {
        statusEl.innerHTML = '<div style="color:red;">Please enter a valid amount.</div>';
        return;
    }

    statusEl.innerHTML = '<div style="color:#1565c0;">Processing swap...</div>';

    try {
        var microAmount = Math.floor(amount * 1000000).toString();
        var deadlineNs  = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

        var offerAsset = {
            info:   { creator_token: { denom: tokenDenom } },
            amount: microAmount
        };

        // Live quote → belief_price, same anti-sandwich guard as the buy.
        var beliefPrice = null;
        var sim = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { simulation: { offer_asset: offerAsset } }
        );
        var expectedOut = parseInt(sim.return_amount);
        if (expectedOut > 0) {
            beliefPrice = (parseInt(microAmount) / expectedOut).toFixed(18);
        }

        // Same simple_swap as a buy — executed on the POOL, with the
        // creator token denom attached as native funds.
        var msg = {
            simple_swap: {
                offer_asset:           offerAsset,
                belief_price:          beliefPrice,
                max_spread:            spreadInput || null,
                // Same semantics as the buy path; leave null unless you've
                // surfaced an explicit override to the user.
                allow_high_max_spread: null,
                to:                    null,
                transaction_deadline:  deadlineNs
            }
        };

        var funds = [{ denom: tokenDenom, amount: microAmount }];

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,   // the pool contract, NOT a token contract
            msg,
            { amount: [], gas: "500000" },
            "Sell Token",
            funds
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Success! Tokens sold.</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#ffebee;border:1px solid #d32f2f;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#d32f2f;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Sell error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 7. Cross-Token Swaps (Router)

Creator tokens never share a pool with each other — every pair trades through OSMO. To let a fan swap *another creator's token* directly into yours, use the **router contract**: it executes the whole route (up to **3 hops**) in one atomic transaction and validates every hop's pool against the factory registry before moving funds.

> **Slippage model:** the router takes **no per-hop spread parameters**. Protection comes from `minimum_receive` on the final token — simulate first with `simulate_multi_hop`, then set `minimum_receive` a tolerance below the simulated output (zero is rejected). If any hop moves the price so the final amount lands short, the entire route reverts; partial swaps cannot strand funds mid-route.

> **Funds are always attached natively.** Both OSMO and creator tokens are bank coins, so whatever the first hop offers is simply attached to the `execute_multi_hop` call as `funds`. There is no CW20 `send` path.

Add the router address to your config block: `routerAddress: "osmo1router_address_here"`.

```html
<script>
async function crossTokenSwap(fromDenom, fromPool, toDenom, toPool, amountMicro, slippagePct) {
    // 1. Build the route: TOKEN_A -> OSMO -> TOKEN_B.
    //    (For OSMO -> TOKEN_B keep only the second hop;
    //     for TOKEN_A -> OSMO keep only the first.)
    var route = [
        {
            pool_addr:        fromPool,
            offer_asset_info: { creator_token: { denom: fromDenom } },
            ask_asset_info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } }
        },
        {
            pool_addr:        toPool,
            offer_asset_info: { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
            ask_asset_info:   { creator_token: { denom: toDenom } }
        }
    ];

    // 2. Simulate to learn the expected output and size minimum_receive.
    var sim = await window.bluechipClient.queryContractSmart(
        BLUECHIP_CONFIG.routerAddress,
        { simulate_multi_hop: { operations: route, offer_amount: amountMicro } }
    );
    console.log("Expected out:", sim.final_amount,
                "per-hop:", sim.intermediate_amounts,
                "impact:", sim.price_impact);

    var slipBps    = Math.round(slippagePct * 100);
    var minReceive = (BigInt(sim.final_amount) * BigInt(10000 - slipBps) / BigInt(10000)).toString();
    var deadlineNs = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

    // 3. Execute — attach whatever the FIRST hop offers as funds
    //    (a creator token's factory/... denom or uosmo; both are
    //    native bank coins).
    var result = await window.bluechipClient.execute(
        window.bluechipAddress,
        BLUECHIP_CONFIG.routerAddress,
        {
            execute_multi_hop: {
                operations:      route,
                minimum_receive: minReceive,
                deadline:        deadlineNs,
                recipient:       null
            }
        },
        { amount: [], gas: "900000" },
        "Cross-Token Swap",
        [{ denom: fromDenom, amount: amountMicro }]
    );

    return result.transactionHash;
}
</script>
```

Both pools in the route must be past their threshold (active pools). Get the router address from the BlueChip team alongside the factory address.

---

## 8. Liquidity — It's a Native Osmosis Pool

Earlier versions of the protocol had their own liquidity-position system (deposit, withdraw, position NFTs, fee collection). **That system is gone.** When a creator pool crosses its threshold, the contract creates and seeds a **native Osmosis GAMM pool**, and:

- The **seed liquidity belongs to no one** — the pool contract holds the
  `gamm/pool/{id}` LP shares itself, permanently. It cannot be pulled,
  rugged, or transferred, and there are **no** `deposit_liquidity`,
  `remove_liquidity`, or `collect_fees` entry points on the contract.
- **Anyone can LP the normal Osmosis way.** Visit
  [app.osmosis.zone](https://app.osmosis.zone), find the pool
  (`OSMO / <creator token>`), and add or remove liquidity there like any
  other Osmosis pool. Positions and LP rewards are managed entirely by
  Osmosis — not by BlueChip contracts, and not by this guide's code.
- **Trading fees accrue to LPs** per Osmosis GAMM rules (the pool is
  created with the protocol's configured swap fee, 0.3% by default).

If your old integration called `deposit_liquidity`, `add_to_position`,
`remove_all_liquidity`, `remove_partial_liquidity`,
`remove_partial_liquidity_by_percent`, `collect_fees`, or the
`position` / `positions_by_owner` queries — delete that code and point
your users at Osmosis instead. The pool id of the native pool is emitted
in the threshold-crossing transaction and can also be inferred from the
pool's reserves queries (Section 10).

---

## 9. Create a Pool

The factory exposes a single creation path — the commit (creator) pool:

- **Commit (creator) pool** — factory `create` message. The new pool mints its own **TokenFactory denom** (`factory/{pool_address}/{subdenom}`) and starts in a funding (commit) phase. Once the configured USD threshold is crossed, 1,200,000 creator tokens are minted and distributed:
   - **500,000** to early subscribers (proportional to their commits)
   - **325,000** to you, the creator
   - **25,000** to the BlueChip protocol
   - **350,000** seeded into the native Osmosis pool as initial liquidity

> **Wire-format note:** The `pool_msg` body carries **only** `pool_token_info`. Every other dial — commit threshold, fee splits, threshold-payout amounts, lock caps, pricing config — is sourced from the factory's stored config and silently overwrites anything a caller tries to send. The `creator_token` entry is a **placeholder with a `denom` key** — the pool overwrites it with its real TokenFactory denom at instantiate.

> **Creation fee:** Pool creation charges a flat creation fee (`pool_creation_fee`, factory config) paid in OSMO. Attach the funds to the call (7th argument to `execute`); the factory verifies the amount via `cw_utils::must_pay`, forwards the fee to the protocol wallet, and refunds any surplus on-chain in the same tx. The snippet below reads the live fee from factory config so you never guess.
>
> **Strict single-denom requirement:** The handler accepts **exactly one** coin entry of `uosmo`. Attaching any other denom alongside (an IBC-wrapped denom, a tokenfactory token) causes the tx to **error at the boundary** rather than silently refund the extras. On error, the bank module auto-returns all attached funds — but the create call fails.
>
> **Fee-disabled case:** If the factory is configured with `pool_creation_fee = 0`, pass an empty `funds` array. Attaching any funds when the fee is disabled also errors.

> **Validation bounds:** Token name must be 3–50 printable ASCII characters; symbol must be 3–12 chars (A–Z, 0–9) with at least one letter; decimals are pinned to 6 (the threshold-payout amounts and mint cap are calibrated for this exact value).

> **Important:** The wallet you use to create the pool becomes the creator wallet. **Do not lose your seed phrase** — BlueChip cannot recover it.

```html
<!-- ============================================================ -->
<!--  CREATE A POOL                                               -->
<!-- ============================================================ -->

<div style="max-width:540px;margin:20px auto;padding:20px;border:2px solid #ff6f00;
            border-radius:12px;background:#fffbf0;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#e65100;">Create Your Creator Pool</h3>

    <div style="padding:12px;background:#fff3e0;border:1px solid #ffb74d;border-radius:8px;
                margin-bottom:16px;font-size:14px;">
        <strong>How it works:</strong>
        <ul style="margin:8px 0 0 0;padding-left:20px;">
            <li>Choose a name and ticker for your token</li>
            <li>Your connected wallet becomes the creator wallet — <strong>DO NOT LOSE IT</strong></li>
            <li>Pool requires $25,000 USD in commits (paid in OSMO) to activate</li>
            <li>You earn 5% of every commit transaction</li>
            <li>Once threshold is met, a native Osmosis pool is created and your token becomes tradeable</li>
            <li>You receive 325,000 creator tokens at threshold crossing</li>
        </ul>
    </div>

    <!-- Commit (creator) pool inputs -->
    <div id="pool-commit-inputs">
        <label style="display:block;margin-bottom:4px;font-weight:bold;">Token Name:</label>
        <input id="pool-token-name" type="text" placeholder="e.g. My Creator Token" maxlength="50"
               style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:4px;" />
        <small style="color:#666;display:block;margin-bottom:12px;">3–50 printable ASCII characters.</small>

        <label style="display:block;margin-bottom:4px;font-weight:bold;">Token Symbol (Ticker):</label>
        <input id="pool-token-symbol" type="text" placeholder="e.g. MCT" maxlength="12"
               style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:4px;
                      text-transform:uppercase;" />
        <small style="color:#666;display:block;margin-bottom:12px;">3–12 chars, A–Z + 0–9, at least one letter.</small>
    </div>

    <div style="padding:12px;background:#e3f2fd;border:1px solid #90caf9;border-radius:8px;
                margin-bottom:16px;font-size:13px;">
        <strong>Sourced from factory config:</strong><br>
        &bull; Commit threshold, fee splits, threshold-payout amounts, lock caps, x/twap pricing config<br>
        &bull; Creator-token decimals are pinned to 6; mint cap pinned at 1,200,000 tokens<br>
        &bull; The flat OSMO creation fee is read live and attached automatically below
    </div>

    <button onclick="handleCreatePool()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#ff6f00;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Create Pool
    </button>

    <div id="create-pool-status" style="margin-top:12px;"></div>
    <div id="create-pool-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleCreatePool() {
    var statusEl = document.getElementById("create-pool-status");
    var txEl     = document.getElementById("create-pool-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    statusEl.innerHTML = '<div style="color:#1565c0;">Creating your pool... This may take a moment.</div>';

    try {
        var tokenName   = document.getElementById("pool-token-name").value.trim();
        var tokenSymbol = document.getElementById("pool-token-symbol").value.trim().toUpperCase();
        if (!tokenName || !tokenSymbol) {
            statusEl.innerHTML = '<div style="color:red;">Please enter both a token name and symbol.</div>';
            return;
        }
        // Mirror the factory's validate_creator_token_info bounds.
        if (tokenName.length < 3 || tokenName.length > 50) {
            statusEl.innerHTML = '<div style="color:red;">Token name must be 3–50 printable ASCII characters.</div>';
            return;
        }
        if (!/^[A-Z0-9]{3,12}$/.test(tokenSymbol) || !/[A-Z]/.test(tokenSymbol)) {
            statusEl.innerHTML = '<div style="color:red;">Token symbol must be 3–12 chars (A–Z, 0–9) with at least one letter.</div>';
            return;
        }

        // Read the flat OSMO creation fee from live factory config and
        // attach exactly that (surplus would be refunded, but exact is
        // cleanest). Zero fee = attach nothing.
        var factoryConfig = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.factoryAddress, { factory: {} }
        );
        var creationFee = (factoryConfig.factory && factoryConfig.factory.pool_creation_fee) || "0";
        var funds = (creationFee !== "0")
            ? [{ denom: BLUECHIP_CONFIG.nativeDenom, amount: creationFee }]
            : [];

        // CreatePool carries ONLY pool_token_info — every other dial
        // (commit threshold, fee splits, threshold payout amounts, lock
        // caps, pricing config) is read from the factory's stored config.
        // Order matters: OSMO at index 0, creator-token placeholder at
        // index 1 (the pool overwrites it with its real factory/... denom).
        var msg = {
            create: {
                pool_msg: {
                    pool_token_info: [
                        { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
                        { creator_token: { denom: "WILL_BE_CREATED_BY_FACTORY" } }
                    ]
                },
                token_info: {
                    name:    tokenName,
                    symbol:  tokenSymbol,
                    // Decimals are pinned to 6; threshold-payout amounts
                    // and the mint cap are calibrated for this value.
                    decimal: 6
                }
            }
        };
        var memo = "Create Commit Pool";

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.factoryAddress,
            msg,
            { amount: [], gas: "2000000" },
            memo,
            funds
        );

        statusEl.innerHTML =
            '<div style="color:#2e7d32;font-weight:bold;">' +
            'Pool created! Share the pool address so people can interact with it.' +
            '</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#fff3e0;border:1px solid #ff6f00;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#ff6f00;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Create pool error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 10. Querying Pool Info (Read-Only)

These queries don't require a wallet connection — they're read-only. You can use them to show pool status on your site.

### Check if Pool Threshold is Reached

```html
<script>
async function checkPoolStatus(poolAddress) {
    // You can use a read-only client for queries
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var status = await client.queryContractSmart(poolAddress, {
        is_fully_commited: {}
    });

    // status is either "fully_committed" or { in_progress: { raised: "...", target: "..." } }
    if (status === "fully_committed") {
        console.log("Pool is active! Trading is enabled.");
        return true;
    } else {
        var raised = parseInt(status.in_progress.raised) / 1000000;
        var target = parseInt(status.in_progress.target) / 1000000;
        console.log("Pool funding: $" + raised.toFixed(2) + " / $" + target.toFixed(2));
        return false;
    }
}
</script>
```

### Get Pool Reserves

Post-migration, `pool_state` reads the **live reserves of the native
Osmosis pool** (zero until the threshold crossing seeds it).
`total_liquidity` and `nft_ownership_accepted` are retained for wire
compatibility only — check the GAMM pool itself for LP-share data.

```html
<script>
async function getPoolState(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var state = await client.queryContractSmart(poolAddress, { pool_state: {} });

    console.log("Reserve 0 (OSMO):",    parseInt(state.reserve0) / 1000000);
    console.log("Reserve 1 (Creator):", parseInt(state.reserve1) / 1000000);

    return state;
}
</script>
```

### Get User's Subscription Info

```html
<script>
async function getSubscriptionInfo(poolAddress, walletAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    // NOTE: the query key is committing_info (double "t", double "m") —
    // it mirrors the contract's CommittingInfo variant exactly.
    var info = await client.queryContractSmart(poolAddress, {
        committing_info: { wallet: walletAddress }
    });

    // Returns null if never committed, or a Committing object
    if (info) {
        console.log("Total paid (USD):",  parseInt(info.total_paid_usd) / 1000000);
        console.log("Total paid (OSMO):", parseInt(info.total_paid_bluechip) / 1000000);
    } else {
        console.log("User has not subscribed yet.");
    }

    return info;
}
</script>
```

### Get the Creator Token Denom from a Pool

```html
<script>
async function getCreatorTokenDenom(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var pairInfo = await client.queryContractSmart(poolAddress, { pair: {} });

    // `asset_infos` is the field on `PoolDetails` that `pair {}` returns.
    // The creator side carries a native TokenFactory DENOM
    // (factory/{pool}/{sub}) — there is no token contract address.
    var assets = pairInfo.asset_infos || [];
    for (var i = 0; i < assets.length; i++) {
        if (assets[i].creator_token) {
            return assets[i].creator_token.denom;
        }
    }
    return null;
}

// A holder's balance is then a plain bank query — no CW20 involved:
async function getCreatorTokenBalance(walletAddress, tokenDenom) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);
    var coin = await client.getBalance(walletAddress, tokenDenom);
    return parseInt(coin.amount) / 1000000;
}
</script>
```

### Creator Earnings & Airdrop Progress (dashboards)

```html
<script>
// Creator-facing rollup: threshold status, and the time-locked
// "excess liquidity" claim (created when the pool raised more OSMO
// than the per-pool lock cap). Claim it once unlocked with
// { claim_creator_excess_liquidity: { transaction_deadline: null } }.
async function getCreatorEarnings(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);
    var e = await client.queryContractSmart(poolAddress, { creator_earnings: {} });
    // { creator_wallet_address, excess: { bluechip_amount, token_amount,
    //   unlock_time, claimable_now } | null, is_threshold_hit,
    //   threshold_crossed_at }
    return e;
}

// Live state of the 500k-token committer airdrop after crossing.
// Returns null when no distribution is active.
async function getDistributionState(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);
    var d = await client.queryContractSmart(poolAddress, { distribution_state: {} });
    if (d) {
        console.log("Remaining recipients:", d.distributions_remaining,
                    "stalled:", d.is_stalled);
    }
    return d;
}
</script>
```

### List Every Pool (factory registry)

```html
<script>
// THE way to answer "what pools exist?" without an indexer. Page with
// start_after = last pool_id; a page shorter than limit is the end.
async function listPools() {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);
    var all = [], startAfter = null, LIMIT = 100;
    for (;;) {
        var page = await client.queryContractSmart(BLUECHIP_CONFIG.factoryAddress, {
            pools: { start_after: startAfter, limit: LIMIT }
        });
        all = all.concat(page.pools);
        if (page.pools.length < LIMIT) break;
        startAfter = page.pools[page.pools.length - 1].pool_id;
    }
    // each entry: { pool_id, pool_addr, pool_token_info: [bluechip, creator_token] }
    return all;
}

// Convert an OSMO amount to USD with the exact same x/twap conversion
// the pools use (micro-units in, micro-USD out):
async function osmoToUsd(microOsmo) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);
    var res = await client.queryContractSmart(BLUECHIP_CONFIG.factoryAddress, {
        pool_factory_query: { convert_native_to_usd: { amount: microOsmo } }
    });
    return res;
}
</script>
```

---

## 11. Granting Special Privileges to Committed Users

Every commit writes a permanent, public record to your pool's ledger: who committed, how much (in USD and OSMO), and when. After the threshold, supporters also receive your creator tokens. Your website can read either of these to give supporters **special privileges** — subscriber-only pages, download links, badges, Discord roles, early access, anything you can gate.

Because every stack is different (static site, WordPress, Node, Discord bot...), this section shows three building blocks, from simplest to most robust. They are plain JavaScript and standard HTTP/WebSocket calls, so they port to any environment.

### Pattern A — Client-Side Gating (good for cosmetic perks)

Read the connected wallet's commit record with the `committing_info` query and show/hide page sections by tier. No server needed — this runs entirely in the visitor's browser.

> **Warning:** client-side checks can be bypassed by anyone comfortable with browser dev tools — and they prove only that a wallet is *connected*, not owned. Use Pattern A for cosmetic perks (badges, styling, shout-outs). For anything valuable (downloads, accounts, paid content), use Pattern B.

```html
<script>
// Tier thresholds in micro-USD (6 decimals): $5,000 / $500.
var TIER_GOLD_MICRO_USD   = 5000000000;
var TIER_SILVER_MICRO_USD = 500000000;

// How recent the last commit must be to count as an "active"
// subscriber. The chain never expires commit records — recency
// is purely your site's policy.
var ACTIVE_WINDOW_DAYS = 30;

async function getSupporterStatus(walletAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    // committing_info returns null if this wallet has never committed,
    // otherwise the wallet's cumulative commit record for this pool.
    var info = await client.queryContractSmart(BLUECHIP_CONFIG.poolAddress, {
        committing_info: { wallet: walletAddress }
    });

    if (!info) {
        return { isSupporter: false, tier: "none", isActive: false };
    }

    // total_paid_usd is micro-USD (1000000 = $1.00), as a string.
    var totalUsd = parseInt(info.total_paid_usd);
    var tier = "bronze";
    if (totalUsd >= TIER_GOLD_MICRO_USD)        tier = "gold";
    else if (totalUsd >= TIER_SILVER_MICRO_USD) tier = "silver";

    // last_committed is a timestamp in NANOSECONDS (as a string).
    var lastCommitMs = parseInt(info.last_committed) / 1000000;
    var ageDays      = (Date.now() - lastCommitMs) / 86400000;
    var isActive     = ageDays <= ACTIVE_WINDOW_DAYS;

    return {
        isSupporter: true,
        tier: tier,
        isActive: isActive,
        totalPaidUsd: totalUsd / 1000000,
        lastCommitted: new Date(lastCommitMs)
    };
}

// Example: unlock page sections after the wallet connects.
async function unlockSupporterContent() {
    if (!window.bluechipAddress) {
        var ok = await connectKeplrWallet();
        if (!ok) return;
    }

    var status = await getSupporterStatus(window.bluechipAddress);

    // Reveal/hide blocks by tier. Give gated blocks these IDs in
    // your HTML: supporter-content, gold-content, etc.
    var supporterEl = document.getElementById("supporter-content");
    if (supporterEl) {
        supporterEl.style.display =
            (status.isSupporter && status.isActive) ? "block" : "none";
    }
    var goldEl = document.getElementById("gold-content");
    if (goldEl) {
        goldEl.style.display = (status.tier === "gold") ? "block" : "none";
    }

    var label = document.getElementById("supporter-status");
    if (label) {
        label.textContent = status.isSupporter
            ? ("Supporter tier: " + status.tier +
               (status.isActive ? " (active)" : " (lapsed)"))
            : "Not a supporter yet — hit Subscribe above!";
    }
}
</script>

<!-- Example gated markup -->
<div id="supporter-status"></div>
<div id="supporter-content" style="display:none;">
    Subscriber-only content here (early videos, downloads, chat invite...)
</div>
<div id="gold-content" style="display:none;">
    Gold-tier extras here.
</div>
```

### Pattern B — Server-Verified Privileges (secure)

The commit ledger is public, so the question your server must answer is not "has this wallet committed?" but "does this visitor *own* that wallet?". The standard solution is an **ADR-36 signature**: Keplr's `signArbitrary` signs a one-time nonce at zero gas cost, your server verifies the signature, then queries the pool over the chain's REST endpoint and grants a role based on the on-chain record.

```javascript
// ============================================================
//  STEP 1 (browser): prove wallet ownership with an ADR-36
//  signature.
// ============================================================
async function loginWithWallet() {
    await window.keplr.enable(BLUECHIP_CONFIG.chainId);

    // 1. Ask your server for a one-time nonce (prevents replay).
    var nonceRes = await fetch("/api/auth/nonce", { method: "POST" });
    var nonce    = (await nonceRes.json()).nonce;

    var signer   = window.getOfflineSigner(BLUECHIP_CONFIG.chainId);
    var accounts = await signer.getAccounts();
    var address  = accounts[0].address;

    // 2. Sign the nonce. signArbitrary = ADR-36: costs no gas and
    //    cannot be replayed as a real transaction.
    var message   = "bluechip-login:" + nonce;
    var signature = await window.keplr.signArbitrary(
        BLUECHIP_CONFIG.chainId, address, message
    );

    // 3. Send to your server for verification.
    var verifyRes = await fetch("/api/auth/verify", {
        method:  "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ address: address, message: message, signature: signature })
    });
    var session = await verifyRes.json();
    console.log("Privileges granted:", session);
}
```

```javascript
// ============================================================
//  STEP 2 (your server — Node.js example, adapt to your stack):
//  verify the signature, then read the commit ledger over the
//  chain's REST (LCD) endpoint and grant privileges by tier.
//
//  npm install @keplr-wallet/cosmos
// ============================================================
const { verifyADR36Amino } = require("@keplr-wallet/cosmos");

const REST_ENDPOINT = "https://lcd.osmosis.zone";
const POOL_ADDRESS  = "osmo1your_pool_address_here";
const BECH32_PREFIX = "osmo";

// Smart-query a contract over REST: the query JSON is base64-encoded
// into the URL. Works from any backend language — only the base64
// and HTTP parts are Node-specific here.
async function queryCommitRecord(walletAddress) {
    const query   = { committing_info: { wallet: walletAddress } };
    const encoded = Buffer.from(JSON.stringify(query)).toString("base64");
    const url     = REST_ENDPOINT +
        "/cosmwasm/wasm/v1/contract/" + POOL_ADDRESS + "/smart/" + encodeURIComponent(encoded);
    const res     = await fetch(url);
    if (!res.ok) throw new Error("LCD query failed: " + res.status);
    return (await res.json()).data;   // null if the wallet never committed
}

// POST /api/auth/verify
async function handleVerify(req, res) {
    const { address, message, signature } = req.body;

    // 1. Check the nonce inside `message` is one you issued and unused,
    //    then mark it spent (not shown — use your session/DB layer).

    // 2. Verify the ADR-36 signature actually binds this address.
    const pubKeyBytes = Buffer.from(signature.pub_key.value, "base64");
    const sigBytes    = Buffer.from(signature.signature, "base64");
    const ok = verifyADR36Amino(
        BECH32_PREFIX, address, message, pubKeyBytes, sigBytes
    );
    if (!ok) return res.status(401).json({ error: "Bad signature" });

    // 3. Wallet ownership proven — now read the on-chain commit record.
    const record = await queryCommitRecord(address);
    if (!record) return res.json({ role: "visitor" });

    // 4. Map the record to YOUR privileges. total_paid_usd is micro-USD.
    const totalUsd = Number(record.total_paid_usd) / 1e6;
    const role = totalUsd >= 5000 ? "gold"
               : totalUsd >= 500  ? "silver"
               : "bronze";

    // 5. Issue your normal session (cookie / JWT / Discord role grant...).
    res.json({ role: role, totalUsd: totalUsd, lastCommitted: record.last_committed });
}
```

### Pattern C — React to Commits in Real Time

Commits emit on-chain events the moment they land. Subscribe to them over the RPC WebSocket to trigger perks instantly — flip on a chat invite, fire a Discord webhook, or thank the supporter by name.

Every commit emits a `wasm` event with these attributes:

| Attribute | Value |
|-----------|-------|
| `action` | `"commit"` |
| `phase` | `"funding"` (pre-threshold) \| `"active"` (post-threshold) \| `"threshold_crossing"` \| `"threshold_hit_exact"` |
| `committer` | wallet address that committed |
| `commit_amount_bluechip` | OSMO committed, in micro-units |
| `total_commit_count` | running commit counter for the pool |
| `pool_contract`, `block_height`, `block_time` | context fields |
| `total_raised_after` / `total_bluechip_raised_after` | pool totals after this commit (funding phase; USD and net OSMO, micro-units) |

> **Note:** events carry the **OSMO** amount only — `commit_amount_usd` is
> no longer emitted. If you need the USD value of a specific commit, query
> `committing_info` for the wallet (its `last_payment_usd` field) or
> convert via the factory's `convert_native_to_usd`.

```javascript
var RPC_WS = BLUECHIP_CONFIG.rpc.replace(/^http/, "ws") + "/websocket";

function watchCommits(onCommit) {
    var ws = new WebSocket(RPC_WS);

    ws.onopen = function () {
        ws.send(JSON.stringify({
            jsonrpc: "2.0",
            method:  "subscribe",
            id:      1,
            params:  {
                query: "tm.event='Tx' AND wasm.action='commit'" +
                       " AND wasm._contract_address='" + BLUECHIP_CONFIG.poolAddress + "'"
            }
        }));
    };

    ws.onmessage = function (msgEvent) {
        var msg = JSON.parse(msgEvent.data);
        // Tendermint flattens attributes into result.events:
        // { "wasm.committer": ["osmo1..."], "wasm.commit_amount_bluechip": ["1000000"], ... }
        var events = msg.result && msg.result.events;
        if (!events || !events["wasm.committer"]) return;

        onCommit({
            committer:  events["wasm.committer"][0],
            phase:      (events["wasm.phase"] || [])[0],
            amountOsmo: parseInt((events["wasm.commit_amount_bluechip"] || ["0"])[0]) / 1000000,
            txHash:     (events["tx.hash"] || [])[0]
        });
    };

    // Reconnect on drop — RPC nodes recycle websocket connections.
    ws.onclose = function () { setTimeout(function () { watchCommits(onCommit); }, 5000); };
    return ws;
}

// Example: grant a perk the moment someone commits.
watchCommits(function (commit) {
    console.log(commit.committer + " committed " + commit.amountOsmo + " OSMO (" + commit.phase + ")");
    // -> POST to your backend, flip a UI flag, fire a Discord webhook, etc.
});

// No websocket? Poll the LCD for recent commit txs instead:
//   GET /cosmos/tx/v1beta1/txs?query=wasm.action='commit'
//        AND wasm._contract_address='<POOL>'&order_by=ORDER_BY_DESC&limit=20
```

> **Design notes:** amounts are micro-units (`total_paid_usd` of `5000000000` = $5,000); `last_committed` is in nanoseconds; commit records never expire on-chain, so "active subscriber" windows (e.g. committed within 30 days) are your site's policy, enforced from `last_committed`. For token-balance-based perks instead, read the wallet's **bank balance** of the creator token's `factory/...` denom (see Section 10) — creator tokens are native coins, so there is no CW20 `balance` query.

---

## 12. Full Working Example Page

Here's a complete, self-contained HTML page you can save and use. It includes wallet connection, subscribe, buy, and sell all on one page.

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>BlueChip - My Creator Page</title>
    <!-- CosmJS has no prebuilt browser bundle; load it as an ES module
         and expose the global the handlers below use. -->
    <script type="module">
        import * as cosmwasm from "https://esm.sh/@cosmjs/cosmwasm-stargate@0.32.4";
        window.CosmWasmClient = cosmwasm;
        window.dispatchEvent(new Event("cosmjs-ready"));
    </script>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
            max-width: 600px;
            margin: 0 auto;
            padding: 20px;
            background: #fafafa;
        }
        h1 { text-align: center; color: #333; }
        .card {
            background: white;
            border-radius: 12px;
            padding: 20px;
            margin-bottom: 20px;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }
        .card h3 { margin-top: 0; }
        input, select {
            width: 100%;
            padding: 10px;
            margin-bottom: 10px;
            border: 1px solid #ddd;
            border-radius: 6px;
            box-sizing: border-box;
            font-size: 14px;
        }
        .btn {
            width: 100%;
            padding: 12px;
            border: none;
            border-radius: 8px;
            font-size: 16px;
            font-weight: bold;
            color: white;
            cursor: pointer;
        }
        .btn-green  { background: #4CAF50; }
        .btn-blue   { background: #1976d2; }
        .btn-red    { background: #d32f2f; }
        .btn:hover  { opacity: 0.9; }
        .status { margin-top: 10px; padding: 8px; border-radius: 6px; }
        .keplr-notice {
            text-align: center;
            padding: 16px;
            background: #fff3cd;
            border: 1px solid #ffc107;
            border-radius: 8px;
            margin-bottom: 20px;
        }
        .keplr-notice a { color: #0d6efd; font-weight: bold; }
    </style>
</head>
<body>
    <h1>My Creator Page</h1>

    <div class="keplr-notice" id="keplr-notice" style="display:none;">
        <strong>Keplr Wallet Required</strong><br>
        To interact with this page, please install the Keplr wallet extension.<br><br>
        <a href="https://www.keplr.app/get" target="_blank">Install Keplr &rarr;</a>
    </div>

    <!-- Wallet Connection -->
    <div class="card">
        <h3>Wallet</h3>
        <button class="btn btn-green" onclick="connectKeplrWallet()">
            Connect Keplr Wallet
        </button>
        <div id="bluechip-wallet-status" style="margin-top:8px;"></div>
        <div id="bluechip-balance" style="margin-top:4px;font-weight:bold;"></div>
    </div>

    <!-- Subscribe -->
    <div class="card">
        <h3>Subscribe</h3>
        <p style="color:#666;font-size:13px;">
            Support this creator. 6% fee: 1% protocol + 5% creator.
        </p>
        <input id="subscribe-amount" type="number" placeholder="Amount (OSMO)" />
        <input id="subscribe-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-green" onclick="handleSubscribe()">Subscribe</button>
        <div id="subscribe-status"></div>
        <div id="subscribe-tx"></div>
    </div>

    <!-- Buy -->
    <div class="card">
        <h3>Buy Creator Tokens</h3>
        <input id="buy-amount" type="number" placeholder="Amount (OSMO to spend)" />
        <input id="buy-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-blue" onclick="handleBuy()">Buy</button>
        <div id="buy-status"></div>
        <div id="buy-tx"></div>
    </div>

    <!-- Sell -->
    <div class="card">
        <h3>Sell Creator Tokens</h3>
        <input id="sell-token-denom" type="text" placeholder="Creator token denom (factory/...)" />
        <input id="sell-amount" type="number" placeholder="Amount (creator tokens)" />
        <input id="sell-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-red" onclick="handleSell()">Sell</button>
        <div id="sell-status"></div>
        <div id="sell-tx"></div>
    </div>

    <p style="text-align:center;color:#999;font-size:12px;">
        Powered by <a href="https://github.com/Bluechip23/bluechip-osmosis-contract"
        target="_blank" style="color:#1976d2;">BlueChip Protocol</a>
    </p>

    <!--
        IMPORTANT: Paste the BLUECHIP_CONFIG block, wallet connection script,
        and all handler functions (handleSubscribe, handleBuy, handleSell)
        from Sections 2-6 of this guide here.
    -->
</body>
</html>
```

---

## 13. Troubleshooting

| Problem | Solution |
|---------|----------|
| **"Please install Keplr extension"** | Install Keplr from [keplr.app/get](https://www.keplr.app/get) and refresh the page |
| **"Failed to connect"** | Make sure you approved Osmosis in Keplr. Try disconnecting and reconnecting |
| **"out of gas"** | Increase the gas limit in the `execute()` call (e.g., change `"500000"` to `"800000"`) |
| **"insufficient funds"** | You need more OSMO. Check your balance in Keplr |
| **"Belief price required" (post-threshold commit)** | Once the pool is active, commits must carry a `belief_price`. Take a live `simulation` quote and set `belief_price = offer / expected_out` (see Section 4) |
| **"Invalid creation funds: ... Send exactly one denom"** | Create-pool requires exactly one coin entry of `uosmo`. Remove any IBC / tokenfactory / stray denoms from the `funds` array before re-broadcasting |
| **"Insufficient commit-pool creation fee"** | The attached OSMO is below the factory's flat `pool_creation_fee`. Query `{ factory: {} }` for the live value and re-attach |
| **"creation fee is disabled; do not attach any funds"** | The factory currently has the creation fee set to zero. Pass an empty `funds` array on these calls |
| **"rate limited"** | Commits have a 13-second cooldown per wallet. Wait and try again |
| **"Route exceeds the maximum of 3 hops"** | The router caps routes at 3 hops. Any creator-token pair needs at most 2 (token → OSMO → token) |
| **"...not registered with the factory" (router)** | A hop's pool address is not in the factory registry. Use addresses from the factory's `pools` query |
| **Router swap reverts on minimum_receive** | Price moved past your tolerance between simulation and execution. Re-quote and retry, or widen slippage slightly |
| **"Commit too small: $X USD (minimum $Y USD ...)"** | Each pool enforces a minimum commit value in USD (separate pre- and post-threshold floors). Increase the amount |
| **"Pool is not fully committed"** | Buy/Sell only work after the pool crosses the USD threshold. Use Subscribe instead |
| **Swap refunded, pool paused ("circuit breaker")** | The pool's liquidity breaker latched (a reserve fell below 25% of its seed). Your offer was refunded in the same tx; trading resumes when the admin unpauses |
| **Calls to `deposit_liquidity` / `collect_fees` / `position` fail** | Those entry points no longer exist — liquidity lives in the native Osmosis pool. LP directly on [app.osmosis.zone](https://app.osmosis.zone) (see Section 8) |
| **Transaction stuck / pending** | The transaction may still be processing. Check the tx hash on [Mintscan](https://www.mintscan.io/osmosis) or another Osmosis explorer |
| **Keplr not detecting on mobile** | Use the Keplr mobile app's built-in browser to visit your site |

---

## 14. Contract Address Reference

These are the addresses you need. Get them from the BlueChip team or a block explorer:

| Identifier | What It Is | Where to Find |
|------------|-----------|---------------|
| **Factory Address** | Creates new pools; registry of all pools | Deployment records / block explorer |
| **Pool Address** | Your specific creator pool | Returned when pool is created (tx hash), or the factory `pools` query |
| **Router Address** | Multi-hop cross-token swaps | Deployment records (deployed alongside the factory) |
| **Creator Token Denom** | Native TokenFactory denom `factory/{pool}/{sub}` | Query the pool's `pair` endpoint |
| **GAMM Pool ID** | The native Osmosis pool seeded at crossing | Threshold-crossing tx events / Osmosis app |

### How to Find Your Creator Token Denom

After your pool is created, you can find the creator token denom by querying:

```javascript
var pairInfo = await client.queryContractSmart("YOUR_POOL_ADDRESS", { pair: {} });
// Look for the creator_token entry in pairInfo.asset_infos —
// its `denom` field is the factory/{pool}/{subdenom} coin.
```

Or check the pool creation transaction on a block explorer — the denom appears in the instantiation events (`create_denom`).

---

**Questions?** Check the [BlueChip GitHub](https://github.com/Bluechip23/bluechip-osmosis-contract) or reach out to the BlueChip community.
