# An agent finds Mizuki and pays for an assessment

This script starts from nothing but Coinbase's public x402 catalog. It searches the catalog for a service that assesses repositories, reads that service's price and input schema from the listing, pays for one call, and prints the result. No endpoint, price, or parameter is hardcoded.

```bash
npm install @solana/kit @x402/core @x402/svm
SOLANA_KEYPAIR_PATH=./wallet.json node discover-and-pay.mjs
```

The wallet needs USDC on Solana mainnet. It does not need SOL: the facilitator sponsors the network fee, so a caller holding only stablecoins can pay.

A run looks like this:

```
catalog    : 15124 resources
found      : https://covenant-x402-seller.onrender.com/x402/mizuki/assess/:owner/:repo
service    : Covenant
price      : 1000 atomic USDC on solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp
input      : {"owner":"open-covenant","repo":"covenant"}

payer      : 5Xmc9QDRLHepaFAq7Bprd4uZbzppQ8df684uXKrekPva
settled    : 3ifwch3i6uFEmoHECiiuJH2729BBGmkuGbPZ3e3hZzY47MwcEy1D68rpUxAzWggBmBvMjNU5s8LCQcJZ7jf3EJKf
assessment : {
  "repository": "open-covenant/covenant",
  "eligible": true,
  "detectedManifest": "pnpm-lock.yaml",
  "validationCommand": "pnpm test"
}
```

That transaction is on Solana mainnet and can be checked on any explorer.

The assessment is the cheap read. To have Mizuki actually fix an issue, quote it and pay the quote: `npx -y mizuki-mcp` exposes that flow to any MCP client, and `mizuki-agent-tools` exposes it to LangChain.
