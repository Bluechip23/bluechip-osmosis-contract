# BlueChip Frontend Integration Guide

> **This guide is for website owners, content creators, and community builders** who want to add BlueChip buttons and features to their own website. You do **not** need to be a programmer — just copy and paste the code blocks below.

---

## Table of Contents

1. [Prerequisites — What You Need First](#1-prerequisites--what-you-need-first)
2. [Quick Start — Add the Script Tags](#2-quick-start--add-the-script-tags)
3. [Connecting to Keplr Wallet](#3-connecting-to-keplr-wallet)
4. [Subscribe Button (Commit)](#4-subscribe-button-commit)
5. [Buy Button (Swap Bluechips for Creator Tokens)](#5-buy-button-swap-bluechips-for-creator-tokens)
6. [Sell Button (Swap Creator Tokens for Bluechips)](#6-sell-button-swap-creator-tokens-for-bluechips)
7. [Cross-Token Swaps (Router)](#7-cross-token-swaps-router)
8. [Add Liquidity](#8-add-liquidity)
9. [Remove Liquidity](#9-remove-liquidity)
10. [Collect Fees](#10-collect-fees)
11. [Create a Pool](#11-create-a-pool)
12. [Querying Pool Info (Read-Only)](#12-querying-pool-info-read-only)
13. [Granting Special Privileges to Committed Users](#13-granting-special-privileges-to-committed-users)
14. [Full Working Example Page](#14-full-working-example-page)
15. [Troubleshooting](#15-troubleshooting)
16. [Contract Address Reference](#16-contract-address-reference)

---

## 1. Prerequisites — What You Need First

### For Your Visitors (People Using Your Website)

Your visitors will need the **Keplr Wallet** browser extension to interact with BlueChip buttons on your site.

**Install Keplr:**
- **Chrome / Brave / Edge:** [Install from Chrome Web Store](https://chrome.google.com/webstore/detail/keplr/dmkamcknogkgcdfhhbddcghachkejeap)
- **Firefox:** [Install from Firefox Add-ons](https://addons.mozilla.org/en-US/firefox/addon/keplr/)
- **Mobile:** [Keplr Mobile App (iOS)](https://apps.apple.com/us/app/keplr-wallet/id1567851089) | [Keplr Mobile App (Android)](https://play.google.com/store/apps/details?id=com.chainapsis.keplr)
- **Official Website:** [https://www.keplr.app/get](https://www.keplr.app/get)

> **Tip:** If a visitor does not have Keplr installed, the code below will show them a friendly message with a link to install it.

### For You (The Website Owner)

You need:
1. A website where you can add HTML and JavaScript (WordPress, Squarespace with code injection, a custom site, etc.)
2. Your **Pool Contract Address** — this is the address of the creator pool on the BlueChip chain (looks like `bluechip1abc...xyz`)
3. Your **Factory Contract Address** — only needed if you want to create new pools

---

## 2. Quick Start

### Fastest path: the BlueChip widget (recommended)

If all you want is a **Subscribe button** and/or **subscriber-gated
content**, skip this whole document and use the prebuilt widget — one
script tag, and the only thing you edit is your pool address:

```html
<script src="https://cdn.jsdelivr.net/gh/Bluechip23/bluechipblockexplorer@main/widget/dist/bluechip-widget.min.js"></script>

<div data-bluechip-subscribe data-pool="bluechip1YOUR_POOL_ADDRESS" data-amount="25"></div>

<div data-bluechip-gate data-pool="bluechip1YOUR_POOL_ADDRESS" data-min-usd="5">
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

Then add this configuration block. **Replace the placeholder values** with your actual addresses:

```html
<script>
// ============================================================
//  BLUECHIP CONFIGURATION — EDIT THESE VALUES
// ============================================================
const BLUECHIP_CONFIG = {
    // Chain settings
    chainId:        "bluechip-3",
    chainName:      "Bluechip Mainnet",
    rpc:            "https://bluechip.rpc.bluechip.link",   // Replace with actual RPC
    rest:           "https://bluechip.api.bluechip.link",   // Replace with actual REST
    nativeDenom:    "ubluechip",
    coinDecimals:   6,

    // Your contract addresses — REPLACE THESE
    factoryAddress: "bluechip1factory_address_here",        // Factory contract
    poolAddress:    "bluechip1your_pool_address_here",      // Your creator pool

    // Keplr chain registration
    bip44:          { coinType: 118 },
    bech32Config: {
        bech32PrefixAccAddr:  "bluechip",
        bech32PrefixAccPub:   "bluechippub",
        bech32PrefixValAddr:  "bluechipvaloper",
        bech32PrefixValPub:   "bluechipvaloperpub",
        bech32PrefixConsAddr: "bluechipvalcons",
        bech32PrefixConsPub:  "bluechipvalconspub",
    },
    currencies: [{
        coinDenom:        "BLUECHIP",
        coinMinimalDenom: "ubluechip",
        coinDecimals:     6,
        coinGeckoId:      "bluechip",
    }],
    feeCurrencies: [{
        coinDenom:        "BLUECHIP",
        coinMinimalDenom: "ubluechip",
        coinDecimals:     6,
        coinGeckoId:      "bluechip",
        gasPriceStep:     { low: 0.01, average: 0.025, high: 0.04 },
    }],
    stakeCurrency: {
        coinDenom:        "BLUECHIP",
        coinMinimalDenom: "ubluechip",
        coinDecimals:     6,
        coinGeckoId:      "bluechip",
    },
};
</script>
```

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
        // Register the BlueChip chain with Keplr
        await window.keplr.experimentalSuggestChain({
            chainId:        BLUECHIP_CONFIG.chainId,
            chainName:      BLUECHIP_CONFIG.chainName,
            rpc:            BLUECHIP_CONFIG.rpc,
            rest:           BLUECHIP_CONFIG.rest,
            bip44:          BLUECHIP_CONFIG.bip44,
            bech32Config:   BLUECHIP_CONFIG.bech32Config,
            currencies:     BLUECHIP_CONFIG.currencies,
            feeCurrencies:  BLUECHIP_CONFIG.feeCurrencies,
            stakeCurrency:  BLUECHIP_CONFIG.stakeCurrency,
        });

        // Enable the chain
        await window.keplr.enable(BLUECHIP_CONFIG.chainId);

        // Get signer and address
        var offlineSigner = window.getOfflineSigner(BLUECHIP_CONFIG.chainId);
        var accounts      = await offlineSigner.getAccounts();
        var address        = accounts[0].address;

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
            balanceEl.textContent = human + " BLUECHIP";
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

The **Subscribe** button lets your fans commit Bluechip tokens to your creator pool. This is how people support you. Before the pool reaches $25,000 USD, commits are recorded in a ledger. After the threshold is crossed, commits are swapped through the AMM and your supporter receives your creator tokens.

**A 6% fee is deducted:** 1% goes to the BlueChip protocol, 5% goes to you the creator.

```html
<!-- ============================================================ -->
<!--  SUBSCRIBE BUTTON                                            -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #4CAF50;
            border-radius:12px;background:#f9fff9;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#2e7d32;">Subscribe (Commit)</h3>
    <p style="color:#666;font-size:14px;">
        Support this creator by committing Bluechip tokens.
        6% fee: 1% protocol + 5% creator.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount (BLUECHIP):
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
        // Convert to micro-units (1 BLUECHIP = 1,000,000 ubluechip)
        var microAmount = Math.floor(amount * 1000000).toString();

        // Check pool threshold status
        var thresholdStatus = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { is_fully_commited: {} }
        );
        var isThresholdCrossed = (thresholdStatus === "fully_committed");

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
                belief_price:         null,
                max_spread:           (isThresholdCrossed && spreadInput) ? spreadInput : null
            }
        };

        // Attach native tokens as funds
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

## 5. Buy Button (Swap Bluechips for Creator Tokens)

The **Buy** button lets people swap their Bluechip tokens for your creator tokens. This only works **after** the pool has crossed the $25,000 threshold and has active liquidity.

```html
<!-- ============================================================ -->
<!--  BUY BUTTON — Swap Bluechips → Creator Tokens                -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #1976d2;
            border-radius:12px;background:#f3f8ff;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#1565c0;">Buy Creator Tokens</h3>
    <p style="color:#666;font-size:14px;">
        Swap your Bluechip tokens for this creator's tokens.
        Only available after the pool threshold is reached.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount (BLUECHIP to spend):
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

        // SimpleSwap: Send native bluechip, receive CW20 creator tokens
        var msg = {
            simple_swap: {
                offer_asset: {
                    info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
                    amount: microAmount
                },
                belief_price:          null,
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

## 6. Sell Button (Swap Creator Tokens for Bluechips)

The **Sell** button lets people swap their creator tokens back into Bluechip tokens. This uses the CW20 `send` mechanism — the tokens are sent to the pool contract with an embedded swap instruction.

> **Important:** Selling creator tokens requires the CW20 token contract address, which is different from the pool address. You can find this by querying the pool's `pair` endpoint (see [Section 12](#12-querying-pool-info-read-only)).

```html
<!-- ============================================================ -->
<!--  SELL BUTTON — Swap Creator Tokens → Bluechips               -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #d32f2f;
            border-radius:12px;background:#fff5f5;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#c62828;">Sell Creator Tokens</h3>
    <p style="color:#666;font-size:14px;">
        Swap creator tokens back to Bluechip tokens.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Creator Token Address:
    </label>
    <input id="sell-token-address" type="text" placeholder="bluechip1abc...xyz"
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

    var tokenAddress = document.getElementById("sell-token-address").value.trim();
    var amount       = parseFloat(document.getElementById("sell-amount").value);
    var spreadInput  = document.getElementById("sell-spread").value;

    if (!tokenAddress) {
        statusEl.innerHTML = '<div style="color:red;">Please enter the creator token address.</div>';
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

        // Build the inner swap hook message
        var hookMsg = {
            swap: {
                belief_price:          null,
                max_spread:            spreadInput || null,
                // Same semantics as simple_swap.allow_high_max_spread; leave
                // null unless you've surfaced an explicit override to the user.
                allow_high_max_spread: null,
                to:                    null,
                transaction_deadline:  deadlineNs
            }
        };

        // Base64-encode the hook message
        var encodedMsg = btoa(JSON.stringify(hookMsg));

        // CW20 Send: send creator tokens to the pool with the swap instruction
        var msg = {
            send: {
                contract: BLUECHIP_CONFIG.poolAddress,   // Pool receives the tokens
                amount:   microAmount,
                msg:      encodedMsg                     // Embedded swap instruction
            }
        };

        // Execute on the CW20 token contract (NOT the pool contract)
        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            tokenAddress,           // The creator token contract address
            msg,
            { amount: [], gas: "500000" },
            "Sell Token",
            []                      // No native funds sent
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

Creator tokens never share a pool with each other — every pair trades through bluechip. To let a fan swap *another creator's token* directly into yours, use the **router contract**: it executes the whole route (up to **3 hops**) in one atomic transaction and validates every hop's pool against the factory registry before moving funds.

> **Slippage model:** the router takes **no per-hop spread parameters**. Protection comes from `minimum_receive` on the final token — simulate first with `simulate_multi_hop`, then set `minimum_receive` a tolerance below the simulated output. If any hop moves the price so the final amount lands short, the entire route reverts; partial swaps cannot strand funds mid-route.

Add the router address to your config block: `routerAddress: "bluechip1router_address_here"`.

```html
<script>
async function crossTokenSwap(fromToken, fromPool, toToken, toPool, amountMicro, slippagePct) {
    // 1. Build the route: TOKEN_A -> bluechip -> TOKEN_B.
    //    (For bluechip -> TOKEN_B keep only the second hop;
    //     for TOKEN_A -> bluechip keep only the first.)
    var route = [
        {
            pool_addr:        fromPool,
            offer_asset_info: { creator_token: { contract_addr: fromToken } },
            ask_asset_info:   { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } }
        },
        {
            pool_addr:        toPool,
            offer_asset_info: { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
            ask_asset_info:   { creator_token: { contract_addr: toToken } }
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

    var hopArgs = {
        operations:      route,
        minimum_receive: minReceive,
        deadline:        deadlineNs,
        recipient:       null
    };

    // 3a. First hop offers a CW20: send the tokens to the router with
    //     the hook embedded (the router takes custody per hop).
    var result = await window.bluechipClient.execute(
        window.bluechipAddress,
        fromToken,                              // execute on the CW20
        {
            send: {
                contract: BLUECHIP_CONFIG.routerAddress,
                amount:   amountMicro,
                msg:      btoa(JSON.stringify({ execute_multi_hop: hopArgs }))
            }
        },
        { amount: [], gas: "900000" },
        "Cross-Token Swap",
        []
    );

    // 3b. If the first hop offers native bluechip instead, call the
    //     router directly and attach the funds:
    //
    //   await window.bluechipClient.execute(
    //       window.bluechipAddress,
    //       BLUECHIP_CONFIG.routerAddress,
    //       { execute_multi_hop: hopArgs },
    //       { amount: [], gas: "900000" },
    //       "Cross-Token Swap",
    //       [{ denom: BLUECHIP_CONFIG.nativeDenom, amount: amountMicro }]
    //   );

    return result.transactionHash;
}
</script>
```

Both pools in the route must be past their threshold (active AMMs). Get the router address from the BlueChip team alongside the factory address.

---

## 8. Add Liquidity

Liquidity providers earn trading fees. When you add liquidity, you receive an NFT that represents your position. You must provide **both** Bluechip tokens and creator tokens in the correct ratio.

> **Note:** Adding liquidity only works **after** the pool threshold has been crossed ($25,000 USD in commits).

There are two steps:
1. **Approve** the pool to spend your creator tokens (CW20 allowance)
2. **Deposit** both tokens into the pool

```html
<!-- ============================================================ -->
<!--  ADD LIQUIDITY                                               -->
<!-- ============================================================ -->

<div style="max-width:540px;margin:20px auto;padding:20px;border:2px solid #7b1fa2;
            border-radius:12px;background:#faf5ff;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#6a1b9a;">Add Liquidity</h3>
    <p style="color:#666;font-size:14px;">
        Provide liquidity to earn trading fees. You'll receive an NFT position.
        Requires both Bluechip and creator tokens.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount 0 — Bluechip (BLUECHIP):
    </label>
    <input id="liq-amount0" type="number" placeholder="e.g. 500"
           style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Amount 1 — Creator Tokens:
    </label>
    <input id="liq-amount1" type="number" placeholder="e.g. 1000"
           style="width:100%;padding:10px;font-size:16px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Slippage Tolerance (%):
    </label>
    <input id="liq-slippage" type="number" value="1" placeholder="1"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleAddLiquidity()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#7b1fa2;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Add Liquidity
    </button>

    <div id="liq-add-status" style="margin-top:12px;"></div>
    <div id="liq-add-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleAddLiquidity() {
    var statusEl = document.getElementById("liq-add-status");
    var txEl     = document.getElementById("liq-add-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var amount0 = parseFloat(document.getElementById("liq-amount0").value);
    var amount1 = parseFloat(document.getElementById("liq-amount1").value);
    var slip    = parseFloat(document.getElementById("liq-slippage").value) || 1;

    if (isNaN(amount0) || amount0 <= 0 || isNaN(amount1) || amount1 <= 0) {
        statusEl.innerHTML = '<div style="color:red;">Please enter valid amounts for both tokens.</div>';
        return;
    }

    statusEl.innerHTML = '<div style="color:#1565c0;">Step 1: Fetching pool info...</div>';

    try {
        var amount0Micro = Math.ceil(amount0 * 1000000).toString();
        var amount1Micro = Math.ceil(amount1 * 1000000).toString();

        // Step 1: Get the creator token address from the pool
        var pairInfo = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress, { pair: {} }
        );

        var tokenAddress   = null;
        var bluechipDenom  = BLUECHIP_CONFIG.nativeDenom;
        // Pair query returns the asset list under `asset_infos` (the
        // serialised field on `PoolDetails` in pool-core/src/state.rs).
        // `pool_token_info` is the *input* field name on the `CreatePool`
        // instantiate message — not the query response — but legacy
        // builds emitted it here too, so read either to stay forward/
        // backward compatible.
        var assets = pairInfo.asset_infos || pairInfo.pool_token_info || [];
        for (var i = 0; i < assets.length; i++) {
            if (assets[i].creator_token) {
                tokenAddress = assets[i].creator_token.contract_addr;
            }
            if (assets[i].bluechip) {
                bluechipDenom = assets[i].bluechip.denom;
            }
        }

        if (!tokenAddress) {
            statusEl.innerHTML = '<div style="color:red;">Error: Could not find creator token in pool.</div>';
            return;
        }

        // Step 2: Check & set CW20 allowance
        statusEl.innerHTML = '<div style="color:#1565c0;">Step 2: Checking token allowance...</div>';

        var allowanceInfo = await window.bluechipClient.queryContractSmart(tokenAddress, {
            allowance: { owner: window.bluechipAddress, spender: BLUECHIP_CONFIG.poolAddress }
        });

        if (parseInt(allowanceInfo.allowance) < parseInt(amount1Micro)) {
            statusEl.innerHTML = '<div style="color:#1565c0;">Step 2: Approving tokens...</div>';
            await window.bluechipClient.execute(
                window.bluechipAddress,
                tokenAddress,
                { increase_allowance: { spender: BLUECHIP_CONFIG.poolAddress, amount: amount1Micro } },
                { amount: [], gas: "200000" },
                "Approve Pool",
                []
            );
        }

        // Step 3: Deposit liquidity
        statusEl.innerHTML = '<div style="color:#1565c0;">Step 3: Depositing liquidity...</div>';

        var slipFactor = 1 - (slip / 100);
        var minAmount0 = Math.floor(parseFloat(amount0Micro) * slipFactor).toString();
        var minAmount1 = Math.floor(parseFloat(amount1Micro) * slipFactor).toString();
        var deadlineNs = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

        var msg = {
            deposit_liquidity: {
                amount0:              amount0Micro,
                amount1:              amount1Micro,
                min_amount0:          minAmount0,
                min_amount1:          minAmount1,
                transaction_deadline: deadlineNs
            }
        };

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,
            msg,
            { amount: [], gas: "500000" },
            "Deposit Liquidity",
            [{ denom: bluechipDenom, amount: amount0Micro }]
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Liquidity added! You received an NFT position.</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#f3e5f5;border:1px solid #7b1fa2;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#7b1fa2;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Add liquidity error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 9. Remove Liquidity

You can remove liquidity three ways:
- **By Amount** — Remove a specific amount of liquidity units
- **By Percentage** — Remove a percentage (e.g., 50%) of your position
- **Remove All** — Withdraw everything

You will need your **Position ID** (the NFT token ID you received when adding liquidity).

```html
<!-- ============================================================ -->
<!--  REMOVE LIQUIDITY                                            -->
<!-- ============================================================ -->

<div style="max-width:540px;margin:20px auto;padding:20px;border:2px solid #e65100;
            border-radius:12px;background:#fff8f0;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#e65100;">Remove Liquidity</h3>
    <p style="color:#666;font-size:14px;">
        Withdraw your liquidity position. You'll receive both Bluechip and creator tokens back.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">Position ID:</label>
    <input id="remove-position-id" type="text" placeholder="Your NFT position ID"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <label style="display:block;margin-bottom:8px;font-weight:bold;">Removal Mode:</label>
    <div style="display:flex;gap:8px;margin-bottom:12px;">
        <button onclick="setRemoveMode('amount')" id="rm-btn-amount"
                style="flex:1;padding:8px;border:2px solid #e65100;border-radius:6px;
                       background:#e65100;color:white;cursor:pointer;font-weight:bold;">
            By Amount
        </button>
        <button onclick="setRemoveMode('percent')" id="rm-btn-percent"
                style="flex:1;padding:8px;border:2px solid #e65100;border-radius:6px;
                       background:white;color:#e65100;cursor:pointer;font-weight:bold;">
            By Percent
        </button>
        <button onclick="setRemoveMode('all')" id="rm-btn-all"
                style="flex:1;padding:8px;border:2px solid #e65100;border-radius:6px;
                       background:white;color:#e65100;cursor:pointer;font-weight:bold;">
            Remove All
        </button>
    </div>

    <div id="remove-amount-section">
        <label style="display:block;margin-bottom:4px;font-weight:bold;">
            Liquidity to Remove:
        </label>
        <input id="remove-amount" type="number" placeholder="e.g. 50000"
               style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />
    </div>

    <div id="remove-percent-section" style="display:none;">
        <label style="display:block;margin-bottom:4px;font-weight:bold;">
            Percentage to Remove (0-100):
        </label>
        <input id="remove-percent" type="number" min="1" max="100" placeholder="e.g. 50"
               style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />
    </div>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">
        Max Ratio Deviation (%):
    </label>
    <input id="remove-deviation" type="number" value="1" placeholder="1"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleRemoveLiquidity()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#e65100;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Remove Liquidity
    </button>

    <div id="remove-status" style="margin-top:12px;"></div>
    <div id="remove-tx" style="margin-top:8px;"></div>
</div>

<script>
var currentRemoveMode = "amount";

function setRemoveMode(mode) {
    currentRemoveMode = mode;
    // Toggle visibility
    document.getElementById("remove-amount-section").style.display  = (mode === "amount")  ? "block" : "none";
    document.getElementById("remove-percent-section").style.display = (mode === "percent") ? "block" : "none";
    // Toggle button styles
    ["amount", "percent", "all"].forEach(function(m) {
        var btn = document.getElementById("rm-btn-" + m);
        if (m === mode) {
            btn.style.background = "#e65100";
            btn.style.color      = "white";
        } else {
            btn.style.background = "white";
            btn.style.color      = "#e65100";
        }
    });
}

async function handleRemoveLiquidity() {
    var statusEl = document.getElementById("remove-status");
    var txEl     = document.getElementById("remove-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var positionId = document.getElementById("remove-position-id").value.trim();
    if (!positionId) {
        statusEl.innerHTML = '<div style="color:red;">Please enter your position ID.</div>';
        return;
    }

    statusEl.innerHTML = '<div style="color:#1565c0;">Verifying ownership...</div>';

    try {
        // Verify the user owns this position
        var positionInfo = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { position: { position_id: positionId } }
        );
        if (positionInfo.owner !== window.bluechipAddress) {
            statusEl.innerHTML = '<div style="color:red;">Error: You do not own this position.</div>';
            return;
        }

        statusEl.innerHTML = '<div style="color:#1565c0;">Removing liquidity...</div>';

        var deviation = parseFloat(document.getElementById("remove-deviation").value) || 1;
        var deviationBps = Math.floor(deviation * 100);
        var deadlineNs   = ((Date.now() + 20 * 60 * 1000) * 1000000).toString();

        var msg;
        if (currentRemoveMode === "all") {
            msg = {
                remove_all_liquidity: {
                    position_id:            positionId,
                    min_amount0:            null,
                    min_amount1:            null,
                    max_ratio_deviation_bps: deviationBps,
                    transaction_deadline:   deadlineNs
                }
            };
        } else if (currentRemoveMode === "percent") {
            var pct = parseInt(document.getElementById("remove-percent").value);
            if (isNaN(pct) || pct < 1 || pct > 100) {
                statusEl.innerHTML = '<div style="color:red;">Percentage must be 1-100.</div>';
                return;
            }
            msg = {
                remove_partial_liquidity_by_percent: {
                    position_id:            positionId,
                    percentage:             pct,
                    min_amount0:            null,
                    min_amount1:            null,
                    max_ratio_deviation_bps: deviationBps,
                    transaction_deadline:   deadlineNs
                }
            };
        } else {
            var removeAmt = parseFloat(document.getElementById("remove-amount").value);
            if (isNaN(removeAmt) || removeAmt <= 0) {
                statusEl.innerHTML = '<div style="color:red;">Please enter a valid amount.</div>';
                return;
            }
            msg = {
                remove_partial_liquidity: {
                    position_id:            positionId,
                    liquidity_to_remove:    Math.floor(removeAmt).toString(),
                    min_amount0:            null,
                    min_amount1:            null,
                    max_ratio_deviation_bps: deviationBps,
                    transaction_deadline:   deadlineNs
                }
            };
        }

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,
            msg,
            { amount: [], gas: "500000" },
            "Remove Liquidity"
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Liquidity removed successfully!</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#fff3e0;border:1px solid #e65100;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#e65100;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Remove liquidity error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 10. Collect Fees

If you have a liquidity position (NFT), you can collect your accumulated trading fees **without** removing your liquidity. Fees are paid out in both Bluechip and creator tokens.

```html
<!-- ============================================================ -->
<!--  COLLECT FEES                                                -->
<!-- ============================================================ -->

<div style="max-width:480px;margin:20px auto;padding:20px;border:2px solid #00897b;
            border-radius:12px;background:#f0faf8;font-family:sans-serif;">

    <h3 style="margin-top:0;color:#00695c;">Collect Fees</h3>
    <p style="color:#666;font-size:14px;">
        Claim accumulated trading fees from your liquidity position
        without withdrawing your liquidity.
    </p>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">Position ID:</label>
    <input id="fees-position-id" type="text" placeholder="Your NFT position ID"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />

    <button onclick="handleCollectFees()"
            style="width:100%;padding:14px;font-size:18px;font-weight:bold;
                   background:#00897b;color:white;border:none;border-radius:8px;
                   cursor:pointer;">
        Collect Fees
    </button>

    <div id="fees-status" style="margin-top:12px;"></div>
    <div id="fees-tx" style="margin-top:8px;"></div>
</div>

<script>
async function handleCollectFees() {
    var statusEl = document.getElementById("fees-status");
    var txEl     = document.getElementById("fees-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var positionId = document.getElementById("fees-position-id").value.trim();
    if (!positionId) {
        statusEl.innerHTML = '<div style="color:red;">Please enter your position ID.</div>';
        return;
    }

    statusEl.innerHTML = '<div style="color:#1565c0;">Verifying ownership...</div>';

    try {
        // Verify the user owns this position
        var positionInfo = await window.bluechipClient.queryContractSmart(
            BLUECHIP_CONFIG.poolAddress,
            { position: { position_id: positionId } }
        );
        if (positionInfo.owner !== window.bluechipAddress) {
            statusEl.innerHTML = '<div style="color:red;">Error: You do not own this position.</div>';
            return;
        }

        // Show unclaimed fees
        var unclaimed0 = (parseInt(positionInfo.unclaimed_fees_0) / 1000000).toFixed(6);
        var unclaimed1 = (parseInt(positionInfo.unclaimed_fees_1) / 1000000).toFixed(6);
        statusEl.innerHTML =
            '<div style="color:#1565c0;">Collecting fees...<br>' +
            'Unclaimed: ' + unclaimed0 + ' BLUECHIP + ' + unclaimed1 + ' Creator Tokens</div>';

        var msg = {
            collect_fees: {
                position_id: positionId
            }
        };

        var result = await window.bluechipClient.execute(
            window.bluechipAddress,
            BLUECHIP_CONFIG.poolAddress,
            msg,
            { amount: [], gas: "400000" },
            "Collect Fees"
        );

        statusEl.innerHTML = '<div style="color:#2e7d32;font-weight:bold;">Fees collected!</div>';
        txEl.innerHTML =
            '<div style="padding:10px;background:#e0f2f1;border:1px solid #00897b;' +
            'border-radius:6px;font-family:monospace;word-break:break-all;position:relative;">' +
            '<strong>Tx Hash:</strong><br>' + result.transactionHash +
            '<button onclick="navigator.clipboard.writeText(\'' + result.transactionHash + '\');' +
            'this.textContent=\'Copied!\';setTimeout(function(){this.textContent=\'Copy\';}.bind(this),2000)"' +
            ' style="position:absolute;top:8px;right:8px;padding:4px 10px;font-size:12px;' +
            'background:#00897b;color:white;border:none;border-radius:4px;cursor:pointer;">Copy</button>' +
            '</div>';

    } catch (err) {
        console.error("Collect fees error:", err);
        statusEl.innerHTML = '<div style="color:red;">Error: ' + err.message + '</div>';
    }
}
</script>
```

---

## 11. Create a Pool

The factory exposes two distinct creation paths. Pick one based on what you want to ship:

- **Commit (creator) pool** — factory `create` message. Mints a fresh CW20 creator token and starts the pool in a funding (commit) phase. Once the configured USD threshold is crossed, 1,200,000 creator tokens are minted and distributed:
   - **500,000** to early subscribers (proportional to their commits)
   - **325,000** to you, the creator
   - **25,000** to the BlueChip protocol
   - **350,000** seeded into the pool as initial liquidity
- **Standard pool** — factory `create_standard_pool` message. Wraps two pre-existing assets in a plain xyk pool. No commit phase, no distribution. **One leg of the pair must be the canonical bluechip denom.**

> **Wire-format note:** The `pool_msg` body now carries **only** `pool_token_info`. Every other dial — commit threshold, fee splits, threshold-payout amounts, lock caps, oracle config — is sourced from the factory's stored config and silently overwrites anything a caller tries to send. Older guides that included `threshold_payout`, `commit_fee_info`, `cw20_token_contract_id`, `factory_to_create_pool_addr`, `pyth_*`, `max_bluechip_lock_per_pool`, `creator_excess_liquidity_lock_days`, or `is_standard_pool` are stale; the factory ignores those fields.

> **Creation fee:** Both paths charge a USD-denominated creation fee paid in canonical bluechip. Attach the funds to the call (7th argument to `execute`); the factory verifies the amount via `cw_utils::must_pay`, forwards the fee to the bluechip wallet, and refunds any surplus on-chain in the same tx.
>
> **Strict single-denom requirement:** The handler accepts **exactly one** coin entry of the canonical bluechip denom. Attaching any other denom alongside (an IBC-wrapped denom, a tokenfactory token, a stray `uatom`) causes the tx to **error at the boundary** rather than silently refund the extras. On error, the bank module auto-returns all attached funds — but the create call fails. Make sure your `funds` array contains only `ubluechip` (or your chain's canonical bluechip denom).
>
> **Fee-disabled case:** If the factory is configured with `standard_pool_creation_fee_usd = 0`, pass an empty `funds` array. Attaching any funds when the fee is disabled also errors.

> **Validation bounds (commit pools):** Token name must be 3–50 printable ASCII characters; symbol must be 3–12 chars (A–Z, 0–9) with at least one letter; decimals are pinned to 6 (the threshold-payout amounts and CW20 mint cap are calibrated for this exact value).

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
            <li>Pool requires $25,000 USD in commits to activate</li>
            <li>You earn 5% of every commit transaction</li>
            <li>Once threshold is met, your token becomes tradeable</li>
            <li>You receive 325,000 creator tokens at threshold crossing</li>
        </ul>
    </div>

    <div style="margin-bottom:12px;">
        <label style="display:flex;align-items:center;gap:8px;cursor:pointer;">
            <input id="pool-standard" type="checkbox"
                   style="width:18px;height:18px;" />
            <span>
                <strong>Standard pool</strong>
                <span style="color:#666;font-size:13px;display:block;">
                    Wrap two pre-existing assets in a plain xyk pool. Skips the commit phase
                    and creator-token mint; you must seed liquidity yourself.
                </span>
            </span>
        </label>
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

    <!-- Standard pool inputs -->
    <div id="pool-standard-inputs" style="display:none;">
        <label style="display:block;margin-bottom:4px;font-weight:bold;">Asset 0:</label>
        <input id="pool-asset0" type="text" value="ubluechip" placeholder="ubluechip or CW20 address"
               style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:4px;" />
        <small style="color:#666;display:block;margin-bottom:12px;">Native bank denom (ubluechip, ibc/...) or CW20 contract address.</small>

        <label style="display:block;margin-bottom:4px;font-weight:bold;">Asset 1:</label>
        <input id="pool-asset1" type="text" placeholder="ubluechip / ibc/... / bluechip1..."
               style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:4px;" />
        <small style="color:#666;display:block;margin-bottom:12px;">One asset MUST be the canonical bluechip denom (<code>ubluechip</code>).</small>

        <label style="display:block;margin-bottom:4px;font-weight:bold;">Pool Label:</label>
        <input id="pool-label" type="text" placeholder="e.g. ATOM/bluechip" maxlength="128"
               style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                      border-radius:6px;box-sizing:border-box;margin-bottom:12px;" />
    </div>

    <label style="display:block;margin-bottom:4px;font-weight:bold;">Creation Fee (ubluechip):</label>
    <input id="pool-creation-fee" type="number" placeholder="micro-units of bluechip"
           style="width:100%;padding:10px;font-size:14px;border:1px solid #ccc;
                  border-radius:6px;box-sizing:border-box;margin-bottom:4px;" />
    <small style="color:#666;display:block;margin-bottom:12px;">USD-denominated; attach the canonical-bluechip equivalent ONLY (no extra denoms). The factory uses <code>must_pay</code> strict-denom validation; surplus is refunded on-chain, but extras error the tx.</small>

    <div style="padding:12px;background:#e3f2fd;border:1px solid #90caf9;border-radius:8px;
                margin-bottom:16px;font-size:13px;">
        <strong>Sourced from factory config (commit pools):</strong><br>
        &bull; Commit threshold, fee splits, threshold-payout amounts, lock caps, oracle config<br>
        &bull; Creator-token decimals are pinned to 6; mint cap pinned at 1,200,000 tokens
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
// Toggle the input groups in lockstep with the standard-pool checkbox so
// the page never displays the wrong set of inputs for the active flow.
document.getElementById("pool-standard").addEventListener("change", function (e) {
    var standard = e.target.checked;
    document.getElementById("pool-commit-inputs").style.display   = standard ? "none"  : "block";
    document.getElementById("pool-standard-inputs").style.display = standard ? "block" : "none";
});

async function handleCreatePool() {
    var statusEl = document.getElementById("create-pool-status");
    var txEl     = document.getElementById("create-pool-tx");
    statusEl.textContent = "";
    txEl.innerHTML       = "";

    if (!window.bluechipClient || !window.bluechipAddress) {
        var connected = await connectKeplrWallet();
        if (!connected) return;
    }

    var isStandard = document.getElementById("pool-standard").checked;

    // Caller-attached creation fee in ubluechip (canonical bluechip denom).
    // The factory verifies it covers the USD-denominated fee converted via
    // the oracle and refunds any surplus on-chain. Leave blank only if the
    // factory has the fee disabled.
    var creationFeeMicro =
        (document.getElementById("pool-creation-fee").value || "").trim();
    var funds = (creationFeeMicro && creationFeeMicro !== "0")
        ? [{ denom: BLUECHIP_CONFIG.nativeDenom, amount: creationFeeMicro }]
        : [];

    statusEl.innerHTML = '<div style="color:#1565c0;">Creating your pool... This may take a moment.</div>';

    try {
        var msg;
        var memo;

        if (!isStandard) {
            // --- Commit (creator) pool ---
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

            // CreatePool now carries ONLY pool_token_info — every other
            // dial (commit threshold, fee splits, threshold payout amounts,
            // lock caps, oracle config) is read from the factory's stored
            // config and silently overwrites anything sent here. Order
            // matters: bluechip at index 0, creator-token sentinel at index 1.
            msg = {
                create: {
                    pool_msg: {
                        pool_token_info: [
                            { bluechip: { denom: BLUECHIP_CONFIG.nativeDenom } },
                            { creator_token: { contract_addr: "WILL_BE_CREATED_BY_FACTORY" } }
                        ]
                    },
                    token_info: {
                        name:    tokenName,
                        symbol:  tokenSymbol,
                        // Decimals are pinned to 6; threshold-payout amounts
                        // and the CW20 mint cap are calibrated for this value.
                        decimal: 6
                    }
                }
            };
            memo = "Create Commit Pool";
        } else {
            // --- Standard (xyk) pool ---
            var asset0 = document.getElementById("pool-asset0").value.trim();
            var asset1 = document.getElementById("pool-asset1").value.trim();
            var label  = document.getElementById("pool-label").value.trim();
            if (!asset0 || !asset1 || !label) {
                statusEl.innerHTML = '<div style="color:red;">Enter both assets and a label for the standard pool.</div>';
                return;
            }
            if (asset0 === asset1) {
                statusEl.innerHTML = '<div style="color:red;">Standard pool cannot pair an asset with itself.</div>';
                return;
            }

            // Heuristic: contract addresses are bech32 (bluechip1.../cosmos1...)
            // and longer than typical native denoms. Anything else is treated
            // as a native bank denom (ubluechip, ibc/... wrapped assets, etc.).
            function buildEntry(s) {
                var looksLikeAddress = s.length > 20 && (s.indexOf("bluechip") === 0 || s.indexOf("cosmos") === 0);
                return looksLikeAddress
                    ? { creator_token: { contract_addr: s } }
                    : { bluechip:      { denom:         s } };
            }
            var entry0 = buildEntry(asset0);
            var entry1 = buildEntry(asset1);

            // Factory enforces that one leg equal the canonical bluechip
            // denom — surface this client-side for a faster error.
            var hasCanonical =
                (entry0.bluechip && entry0.bluechip.denom === BLUECHIP_CONFIG.nativeDenom) ||
                (entry1.bluechip && entry1.bluechip.denom === BLUECHIP_CONFIG.nativeDenom);
            if (!hasCanonical) {
                statusEl.innerHTML =
                    '<div style="color:red;">One asset must be the canonical bluechip denom (' +
                    BLUECHIP_CONFIG.nativeDenom + ').</div>';
                return;
            }

            msg = {
                create_standard_pool: {
                    pool_token_info: [entry0, entry1],
                    label: label
                }
            };
            memo = "Create Standard Pool";
        }

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

## 12. Querying Pool Info (Read-Only)

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

### Get Pool Reserves and Liquidity

```html
<script>
async function getPoolState(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var state = await client.queryContractSmart(poolAddress, { pool_state: {} });

    console.log("Reserve 0 (Bluechip):", parseInt(state.reserve0) / 1000000);
    console.log("Reserve 1 (Creator):",  parseInt(state.reserve1) / 1000000);
    console.log("Total Liquidity:",      parseInt(state.total_liquidity) / 1000000);

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
        console.log("Total paid (USD):", parseInt(info.total_paid_usd) / 1000000);
        console.log("Total paid (BLUECHIP):", parseInt(info.total_paid_bluechip) / 1000000);
    } else {
        console.log("User has not subscribed yet.");
    }

    return info;
}
</script>
```

### Get User's Liquidity Positions

```html
<script>
async function getMyPositions(poolAddress, walletAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var result = await client.queryContractSmart(poolAddress, {
        positions_by_owner: { owner: walletAddress }
    });

    result.positions.forEach(function(pos) {
        console.log("Position ID:", pos.position_id);
        console.log("  Liquidity:", parseInt(pos.liquidity) / 1000000);
        console.log("  Unclaimed Fees 0:", parseInt(pos.unclaimed_fees_0) / 1000000);
        console.log("  Unclaimed Fees 1:", parseInt(pos.unclaimed_fees_1) / 1000000);
    });

    return result.positions;
}
</script>
```

### Get Creator Token Address from Pool

```html
<script>
async function getCreatorTokenAddress(poolAddress) {
    var client = await CosmWasmClient.CosmWasmClient.connect(BLUECHIP_CONFIG.rpc);

    var pairInfo = await client.queryContractSmart(poolAddress, { pair: {} });

    // `asset_infos` is the field on `PoolDetails` that `Pair {}` returns.
    // Some legacy builds emitted `pool_token_info` here too (it's the
    // input field name on `CreatePool`); read either for compatibility.
    var assets = pairInfo.asset_infos || pairInfo.pool_token_info || [];
    for (var i = 0; i < assets.length; i++) {
        if (assets[i].creator_token) {
            return assets[i].creator_token.contract_addr;
        }
    }
    return null;
}
</script>
```

---

## 13. Granting Special Privileges to Committed Users

Every commit writes a permanent, public record to your pool's ledger: who committed, how much (in USD and bluechip), and when. After the threshold, supporters also receive your creator tokens. Your website can read either of these to give supporters **special privileges** — subscriber-only pages, download links, badges, Discord roles, early access, anything you can gate.

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

const REST_ENDPOINT = "https://bluechip.api.bluechip.link";
const POOL_ADDRESS  = "bluechip1your_pool_address_here";
const BECH32_PREFIX = "bluechip";

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
| `commit_amount_bluechip` / `commit_amount_usd` | amounts in micro-units |
| `total_commit_count` | running commit counter for the pool |
| `pool_contract`, `block_height`, `block_time` | context fields |

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
        // { "wasm.committer": ["bluechip1..."], "wasm.commit_amount_usd": ["1000000"], ... }
        var events = msg.result && msg.result.events;
        if (!events || !events["wasm.committer"]) return;

        onCommit({
            committer: events["wasm.committer"][0],
            phase:     (events["wasm.phase"] || [])[0],
            amountUsd: parseInt((events["wasm.commit_amount_usd"] || ["0"])[0]) / 1000000,
            txHash:    (events["tx.hash"] || [])[0]
        });
    };

    // Reconnect on drop — RPC nodes recycle websocket connections.
    ws.onclose = function () { setTimeout(function () { watchCommits(onCommit); }, 5000); };
    return ws;
}

// Example: grant a perk the moment someone commits.
watchCommits(function (commit) {
    console.log(commit.committer + " committed $" + commit.amountUsd + " (" + commit.phase + ")");
    // -> POST to your backend, flip a UI flag, fire a Discord webhook, etc.
});

// No websocket? Poll the LCD for recent commit txs instead:
//   GET /cosmos/tx/v1beta1/txs?query=wasm.action='commit'
//        AND wasm._contract_address='<POOL>'&order_by=ORDER_BY_DESC&limit=20
```

> **Design notes:** amounts are micro-units (`total_paid_usd` of `5000000000` = $5,000); `last_committed` is in nanoseconds; commit records never expire on-chain, so "active subscriber" windows (e.g. committed within 30 days) are your site's policy, enforced from `last_committed`. For token-balance-based perks instead, query the creator token's CW20 `balance` endpoint the same way.

---

## 14. Full Working Example Page

Here's a complete, self-contained HTML page you can save and use. It includes wallet connection, subscribe, buy, sell, and fee collection all on one page.

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
        .btn-teal   { background: #00897b; }
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
        <input id="subscribe-amount" type="number" placeholder="Amount (BLUECHIP)" />
        <input id="subscribe-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-green" onclick="handleSubscribe()">Subscribe</button>
        <div id="subscribe-status"></div>
        <div id="subscribe-tx"></div>
    </div>

    <!-- Buy -->
    <div class="card">
        <h3>Buy Creator Tokens</h3>
        <input id="buy-amount" type="number" placeholder="Amount (BLUECHIP to spend)" />
        <input id="buy-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-blue" onclick="handleBuy()">Buy</button>
        <div id="buy-status"></div>
        <div id="buy-tx"></div>
    </div>

    <!-- Sell -->
    <div class="card">
        <h3>Sell Creator Tokens</h3>
        <input id="sell-token-address" type="text" placeholder="Creator token address" />
        <input id="sell-amount" type="number" placeholder="Amount (creator tokens)" />
        <input id="sell-spread" type="text" value="0.005" placeholder="Max spread" />
        <button class="btn btn-red" onclick="handleSell()">Sell</button>
        <div id="sell-status"></div>
        <div id="sell-tx"></div>
    </div>

    <!-- Collect Fees -->
    <div class="card">
        <h3>Collect Fees</h3>
        <input id="fees-position-id" type="text" placeholder="Position ID" />
        <button class="btn btn-teal" onclick="handleCollectFees()">Collect Fees</button>
        <div id="fees-status"></div>
        <div id="fees-tx"></div>
    </div>

    <p style="text-align:center;color:#999;font-size:12px;">
        Powered by <a href="https://github.com/Bluechip23/bluechip-osmosis-contract"
        target="_blank" style="color:#1976d2;">BlueChip Protocol</a>
    </p>

    <!--
        IMPORTANT: Paste the BLUECHIP_CONFIG block, wallet connection script,
        and all handler functions (handleSubscribe, handleBuy, handleSell,
        handleCollectFees) from Sections 2-9 of this guide here.
    -->
</body>
</html>
```

---

## 15. Troubleshooting

| Problem | Solution |
|---------|----------|
| **"Please install Keplr extension"** | Install Keplr from [keplr.app/get](https://www.keplr.app/get) and refresh the page |
| **"Failed to connect"** | Make sure you've approved the BlueChip chain in Keplr. Try disconnecting and reconnecting |
| **"out of gas"** | Increase the gas limit in the `execute()` call (e.g., change `"500000"` to `"800000"`) |
| **"insufficient funds"** | You need more BLUECHIP tokens. Check your balance in Keplr |
| **"Invalid creation funds: ... Send exactly one denom"** | Create-pool requires exactly one coin entry of the canonical bluechip denom. Remove any IBC / tokenfactory / stray denoms from the `funds` array before re-broadcasting |
| **"Insufficient commit-pool creation fee" / "Insufficient creation fee"** | The attached bluechip amount is below the oracle-derived USD fee. Re-query the required amount (it changes with bluechip's USD price) and re-attach |
| **"creation fee is disabled; do not attach any funds"** | The factory currently has the creation fee set to zero. Pass an empty `funds` array on these calls |
| **"rate limited"** | Commits have a 13-second cooldown per wallet. Wait and try again |
| **"Route exceeds the maximum of 3 hops"** | The router caps routes at 3 hops. Any creator-token pair needs at most 2 (token → bluechip → token) |
| **"...not registered with the factory" (router)** | A hop's pool address is not in the factory registry. Use addresses from the factory's `pools` query |
| **Router swap reverts on minimum_receive** | Price moved past your tolerance between simulation and execution. Re-quote and retry, or widen slippage slightly |
| **"Commit too small: $X USD (minimum $Y USD ...)"** | Each pool enforces a minimum commit value in USD (separate pre- and post-threshold floors). Increase the amount |
| **"Pool is not fully committed"** | Buy/Sell only work after the pool crosses the $25,000 threshold. Use Subscribe instead |
| **"You do not own this position"** | Double-check your Position ID. Query `positions_by_owner` to find your positions |
| **Transaction stuck / pending** | The transaction may still be processing. Check the tx hash on your block explorer |
| **Keplr not detecting on mobile** | Use the Keplr mobile app's built-in browser to visit your site |

---

## 16. Contract Address Reference

These are the addresses you need. Get them from the BlueChip team or your block explorer:

| Address | What It Is | Where to Find |
|---------|-----------|---------------|
| **Factory Address** | Creates new pools | Deployment records / block explorer |
| **Pool Address** | Your specific creator pool | Returned when pool is created (tx hash) |
| **Creator Token Address** | The CW20 token for your pool | Query pool's `pair` endpoint |
| **Position NFT Address** | NFT contract for LP positions | Part of pool creation response |

### How to Find Your Creator Token Address

After your pool is created, you can find the creator token address by querying:

```javascript
var pairInfo = await client.queryContractSmart("YOUR_POOL_ADDRESS", { pair: {} });
// Look for the creator_token entry in pairInfo.asset_infos
// (legacy builds also surfaced it as pairInfo.pool_token_info, which is
// the input field name on CreatePool — read either for compatibility)
```

Or check the pool creation transaction on your block explorer — the token contract address appears in the instantiation events.

---

**Questions?** Check the [BlueChip GitHub](https://github.com/Bluechip23/bluechip-osmosis-contract) or reach out to the BlueChip community.

