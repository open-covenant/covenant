# Hermes MCP Bridge

`@covenant/hermes-mcp-bridge` is a stdio MCP server that exposes generic Hermes agent execution through the Hermes API Server.

It complements Hermes' native MCP server. Use Hermes' native `hermes mcp serve` for messaging and session bridge tools, and this package for generic agent runs.

## Configuration

- `HERMES_API_BASE_URL`: Hermes API Server base URL. Defaults to `http://127.0.0.1:8642/v1`.
- `HERMES_API_KEY`: optional bearer token sent to the Hermes API Server. Set this to the server's `API_SERVER_KEY` value when auth is enabled.

## Tools

- `hermes_health`
- `hermes_capabilities`
- `hermes_run`
- `hermes_run_status`
- `hermes_run_events`
- `hermes_stop`

`hermes_run` sends `POST /v1/runs` to Hermes. Pass `input`; `prompt` is accepted as a compatibility alias and is forwarded as `input`.

## Covenant MCP Config

```toml
[[mcp.server]]
name = "hermes-messaging"
command = "hermes"
args = ["mcp", "serve"]
tool_prefix = "hermes"

[[mcp.server]]
name = "hermes-agent"
command = "pnpm"
args = ["--filter", "@covenant/hermes-mcp-bridge", "start"]
tool_prefix = "hermes_agent"
env = { HERMES_API_BASE_URL = "http://127.0.0.1:8642/v1" }
```

This bridge is trusted-local and host-capable if the Hermes API Server is host-capable. It is not a sandbox boundary.
