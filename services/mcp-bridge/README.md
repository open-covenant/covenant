# Covenant MCP Bridge

MCP server for discovering Covenant agents, preparing Solana-native protocol instruction descriptors, calling the local `covenantd` HTTP gateway through daemon-backed tools, and exposing the verifiable-action surface a Covenant skill runs under.

## Verifiable-action tools

The four actions that make an agent's on-chain work verifiable, exposed as MCP tools:

| Action | Tool | What it does |
|---|---|---|
| propose-tx | `solana_propose_tx` | Build an **unsigned** Solana proposal (`programId`, `instruction`, `accounts`, `data`) in the `@covenant/sdk` bundle shape. Devnet by default. Never signs or sends — the daemon broker simulates, capability-checks, and signs downstream. |
| grant-capability | `daemon_capabilities_grant` | Grant the signed capability a run must hold before the daemon will sign a matching `chain.tx.{program}.{ix}`. |
| query-audit | `daemon_audit_recent` | Read the hash-chained audit events the run produced. |
| fetch-witness-proof | `fetch_witness_proof` | Fetch the separately-keyed verifier's verdict over a window of recent audit events — the witness proof a consumer light-verifies. |

`solana_propose_tx` is a pure builder (no key, no network); the other three forward to `covenantd`, which keeps signing, capabilities, and audit as the authority boundary.

## Run

```bash
pnpm --filter @covenant/mcp-bridge build
pnpm --filter @covenant/mcp-bridge start
```

## Environment

- `COVENANT_SOLANA_CLUSTER`
- `COVENANT_SOLANA_RPC_URL`
- `COVENANT_SOLANA_WS_URL`
- `COVENANT_PROTOCOL_PROGRAM_ID`
- `COVNT_MINT`
- `COVENANT_HTTP_URL`: Covenant daemon HTTP gateway. Defaults to `http://127.0.0.1:8421`.
- `COVENANT_AUTH_TOKEN`: bearer token for daemon-backed tools. If unset, the bridge reads `$COVENANT_HOME/peers/operator.token` or `$HOME/.covenant/peers/operator.token`.

## Daemon Tools

Read tools:

- `daemon_health`
- `daemon_tools_list`
- `daemon_memory_recent`
- `daemon_memory_search`
- `daemon_audit_recent`
- `daemon_receipts_recent`
- `daemon_a2a_status`
- `daemon_budget_debits`
- `daemon_chain_status`
- `fetch_witness_proof`

Action tools:

- `daemon_submit_intent`
- `daemon_tools_call`
- `daemon_a2a_send_task`
- `daemon_a2a_post_result`
- `daemon_capabilities_grant`
- `daemon_capabilities_revoke`
- `daemon_flush_receipts`

The bridge never writes Covenant state directly. Privileged operations are forwarded to `covenantd`, where peer auth, capabilities, audit, budget debits, memory writes, and receipts remain the authority boundary.

## Hermes Config

Hermes can consume Covenant through this MCP server:

```yaml
mcp_servers:
  covenant:
    command: "pnpm"
    args: ["--filter", "@covenant/mcp-bridge", "start"]
```

This is trusted-local integration. A Hermes process with this bridge gets the daemon authority of its bearer token; use the operator token only for local development.

## Hosted endpoint pattern

The bridge ships a stdio transport (`covenant-mcp`). A hosted deployment wraps that
same tool surface behind a stable HTTP/SSE MCP endpoint — the pattern Solana's own
`mcp.solana.com/mcp` follows — so a remote agent can reach Covenant without a local
checkout:

```
agent ──HTTP/SSE──> https://mcp.opencovenant.org/mcp ──stdio──> covenant-mcp ──> covenantd
```

The hosted process still authenticates to `covenantd` with an operator bearer token
and stays pinned to **devnet**; it is not a way to bypass the capability or signing
boundary. A hosted endpoint is a deployment of this package, not a separate server —
no hosted URL is implied to be live here.

## Devnet auto-install

To register the bridge with an agent runtime, pin the network to devnet:

```jsonc
{
  "mcpServers": {
    "covenant": {
      "command": "pnpm",
      "args": ["--filter", "@covenant/mcp-bridge", "start"],
      "env": {
        "COVENANT_SOLANA_CLUSTER": "devnet",
        "COVENANT_HTTP_URL": "http://127.0.0.1:8421"
      }
    }
  }
}
```

Never set `COVENANT_SOLANA_CLUSTER` to `mainnet` for skill-driven runs — the
on-chain skill pipeline is devnet-only, and mainnet promotion is a separate gated
milestone.
