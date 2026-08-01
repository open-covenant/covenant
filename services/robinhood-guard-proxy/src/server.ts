#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { connectStdio, createGuardServer, McpUpstream } from "./proxy.js";
import { GuardAttestor } from "./receipt.js";
import type { TradingPolicy } from "./policy.js";

const DEFAULT_URL = "https://agent.robinhood.com/mcp/trading";

function loadPolicy(): TradingPolicy {
  const path = process.env.COVENANT_TRADING_POLICY;
  if (!path) {
    // No policy configured means no order may pass: any positive value trips
    // the $0 per-order cap, and an unpriced order fails closed.
    process.stderr.write("robinhood-guard: COVENANT_TRADING_POLICY unset; defaulting to deny-all-orders\n");
    return { caps: { per_order_usd: 0 } };
  }
  try {
    return JSON.parse(readFileSync(path, "utf8")) as TradingPolicy;
  } catch (e) {
    process.stderr.write(`robinhood-guard: cannot read policy ${path}: ${e instanceof Error ? e.message : String(e)}\n`);
    process.exit(1);
  }
}

const url = process.env.ROBINHOOD_MCP_URL ?? DEFAULT_URL;
const headers: Record<string, string> = {};
const auth = process.env.ROBINHOOD_MCP_AUTH?.trim();
if (auth) headers.Authorization = auth.startsWith("Bearer ") ? auth : `Bearer ${auth}`;

const attestor = GuardAttestor.fromEnv();
const orderTools = process.env.COVENANT_GUARD_ORDER_TOOLS?.split(",").map((s) => s.trim()).filter(Boolean);

process.stderr.write(`robinhood-guard: proxying ${url}; receipts signed by ${attestor.pubkeyB58}\n`);

const server = createGuardServer({
  upstream: new McpUpstream(url, headers),
  policy: loadPolicy(),
  attestor,
  orderTools,
  onDecision: (d) =>
    process.stderr.write(`[guard] ${d.payload.decision} ${d.payload.order.symbol} ${d.payload.reason ?? ""}\n`),
});

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on("unhandledRejection", (reason) => {
    process.stderr.write(`robinhood-guard: unhandled rejection: ${reason instanceof Error ? reason.message : String(reason)}\n`);
    process.exit(1);
  });
  await connectStdio(server);
}
