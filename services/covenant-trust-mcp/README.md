# Covenant Trust MCP

A zero-install MCP for agents. Add it to Claude Code or Codex and, with no prior
setup and no credentials, the agent can:

- `covenant_reputation` — a Solana wallet's reputation (0-1000) grounded in public
  on-chain USDC settlements (jobs settled, distinct counterparties, inbound volume).
- `covenant_agent_passport` — an MPL Core agent asset's on-chain identity: registered
  in the Agent Identity registry, in the Covenant collection, and its attestation.
- `covenant_verify` — verify a Covenant-signed attestation (ed25519 over a
  domain-separated SHA-256 of the canonical payload); tampering fails.

Every tool is a pure read or pure crypto. No keys, no local state, no payment.
The reputation and passport logic is reused verbatim from the x402-seller so the
free MCP and the paid product never drift.

## Run

    npm install && npm run build
    COVENANT_SOLANA_MAINNET_RPC_URL=<das-rpc> node dist/server.js          # HTTP, POST /mcp
    COVENANT_SOLANA_MAINNET_RPC_URL=<das-rpc> node dist/server.js --stdio  # stdio (local/npx)

## Add it (once hosted at mcp.opencovenant.org)

    claude mcp add --transport http covenant https://mcp.opencovenant.org/mcp
    codex  mcp add --transport http covenant https://mcp.opencovenant.org/mcp

## Env

- `COVENANT_SOLANA_MAINNET_RPC_URL` — DAS-capable Solana RPC (Helius). Required for
  reputation + passport; verify needs none.
- `PORT` (default 8930), `RPC_TIMEOUT_MS` (9000), `REPUTATION_LIMIT` (100).

Verified end-to-end (stdio + HTTP) against live mainnet with the MCP client SDK;
see `test-client.mjs`.
