# Covenant MCP Bridge

MCP server for discovering Covenant agents, preparing Solana-native protocol instruction descriptors, and calling the local `covenantd` HTTP gateway through daemon-backed tools.

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
