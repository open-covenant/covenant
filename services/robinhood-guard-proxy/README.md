# @covenant-org/robinhood-guard-proxy

A Covenant Guard MCP proxy that sits in front of Robinhood's agentic trading MCP.
Your agent connects to this instead of Robinhood directly. Reads and non-order
tools pass straight through; an order tool is checked against your policy first,
so a refused order never reaches the venue, and every decision (placed or
refused) is signed.

You bring your own Robinhood agentic account. The proxy holds no funds and runs
locally.

## Use it

```bash
claude mcp add robinhood-guard --transport stdio -- npx -y @covenant-org/robinhood-guard-proxy
```

Environment:

- `ROBINHOOD_MCP_URL` — upstream agentic MCP (default `https://agent.robinhood.com/mcp/trading`)
- `ROBINHOOD_MCP_AUTH` — bearer token forwarded to the upstream
- `COVENANT_TRADING_POLICY` — path to a policy JSON (unset ⇒ deny all orders)
- `COVENANT_GUARD_ATTESTOR_KEYPAIR` — base64 ed25519 seed for signing receipts (unset ⇒ ephemeral, pubkey logged)
- `COVENANT_GUARD_ORDER_TOOLS` — comma-separated exact order-tool names, overriding the heuristic

Policy JSON (same schema as the covenant-robinhood crate):

```json
{
  "venue": "robinhood",
  "caps": { "per_order_usd": 500, "daily_notional_usd": 2000 },
  "risk": { "daily_loss_stop_usd": 300 },
  "universe": { "allow": ["BTC-USD", "ETH-USD"], "sides": ["buy"] },
  "order_types": ["market"],
  "rate": { "max_orders_per_min": 10 },
  "approvals": { "require_human_over_usd": 400 }
}
```

## To confirm against a live account

Robinhood's agentic MCP schema isn't public, so two things need a real (US-only)
agentic account to pin down, both overridable without a code change:

- the exact order-tool names — set `COVENANT_GUARD_ORDER_TOOLS` if the heuristic
  misses one (it errs toward intercepting anything that looks like an order);
- how the order value arrives in the tool arguments — the extractor reads common
  dollar-amount fields and fails closed when it can't determine a value and a USD
  cap is set.
