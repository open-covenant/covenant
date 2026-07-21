# @covenant/robinhood-mcp-bridge

MCP server exposing governed Robinhood crypto trading through `covenantd`.

Every tool proxies to the daemon `/tools/call` gate, which runs the Covenant
policy capability and signs the request in the `covenant-robinhood-signer`
sidecar. The private key never enters this process, and the governed order takes
the same gated path as the reads.

Add it to an MCP client:

    claude mcp add robinhood --transport stdio -- node dist/server.js

Environment:

- `COVENANT_HTTP_URL` — covenantd HTTP gateway (default `http://127.0.0.1:8421`)
- `COVENANT_AUTH_TOKEN` or `COVENANT_HOME` — daemon bearer auth

Tools: `robinhood_account`, `robinhood_holdings`, `robinhood_quote`,
`robinhood_estimated_price`, `robinhood_governed_order`, `robinhood_cancel_order`,
`robinhood_receipts`, `robinhood_reputation`.
