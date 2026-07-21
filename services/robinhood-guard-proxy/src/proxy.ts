// The Guard proxy core: an MCP server that upstreams to another MCP server
// (Robinhood's agentic endpoint) and intercepts order tools. Reads and
// non-order tools pass through untouched. An order tool runs the local policy
// first: refused orders never reach the venue, and every decision, placed or
// refused, is signed. `guardedCall` is pure w.r.t. the transport so it can be
// tested against a mock upstream.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import type { CallToolResult, Tool } from "@modelcontextprotocol/sdk/types.js";

import { authorize, extractOrder, isOrderTool, newState, recordOrder, type PolicyState, type TradingPolicy } from "./policy.js";
import { GuardAttestor, type Decision, type SignedDecision } from "./receipt.js";

export interface Upstream {
  listTools(): Promise<Tool[]>;
  callTool(name: string, args: Record<string, unknown>): Promise<CallToolResult>;
  close(): Promise<void>;
}

/** The real upstream: Robinhood's hosted agentic MCP over streamable HTTP. */
export class McpUpstream implements Upstream {
  private client: Client;
  private connected = false;

  constructor(
    private readonly url: string,
    private readonly headers: Record<string, string> = {},
  ) {
    this.client = new Client({ name: "covenant-robinhood-guard", version: "0.1.0" }, { capabilities: {} });
  }

  private async ensure(): Promise<void> {
    if (this.connected) return;
    const transport = new StreamableHTTPClientTransport(new URL(this.url), { requestInit: { headers: this.headers } });
    await this.client.connect(transport);
    this.connected = true;
  }

  async listTools(): Promise<Tool[]> {
    await this.ensure();
    return (await this.client.listTools()).tools;
  }

  async callTool(name: string, args: Record<string, unknown>): Promise<CallToolResult> {
    await this.ensure();
    return (await this.client.callTool({ name, arguments: args })) as CallToolResult;
  }

  async close(): Promise<void> {
    if (this.connected) await this.client.close();
  }
}

export interface GuardContext {
  upstream: Upstream;
  policy: TradingPolicy;
  attestor: GuardAttestor;
  /** Override the order-tool match once Robinhood's real tool names are known. */
  orderTools?: string[];
  onDecision?: (d: SignedDecision) => void;
  /** Test seam: unix seconds. */
  now?: () => number;
}

function text(value: unknown, isError = false): CallToolResult {
  return { content: [{ type: "text", text: typeof value === "string" ? value : JSON.stringify(value, null, 2) }], isError };
}

export async function guardedCall(
  ctx: GuardContext,
  state: PolicyState,
  name: string,
  args: Record<string, unknown>,
): Promise<CallToolResult> {
  if (!isOrderTool(name, ctx.orderTools)) {
    return ctx.upstream.callTool(name, args);
  }

  const now = (ctx.now ?? (() => Math.floor(Date.now() / 1000)))();
  const parsed = extractOrder(args);

  const emit = (decision: Decision, reason?: string): SignedDecision => {
    const order = parsed?.order ?? { symbol: String(args.symbol ?? "unknown"), side: "buy" };
    const signed = ctx.attestor.sign({
      schema: "covenant.robinhood.guard.v1",
      venue: ctx.policy.venue ?? "robinhood",
      agent: "mcp-agent",
      decision,
      reason,
      order: {
        symbol: order.symbol,
        side: order.side,
        type: order.type,
        quantity: order.quantity,
        notionalUsd: parsed?.notionalUsd ?? undefined,
      },
      ts: now,
    });
    ctx.onDecision?.(signed);
    return signed;
  };

  if (!parsed) {
    const receipt = emit("blocked", "could not parse an order from the tool arguments; refusing (fail-closed)");
    return text({ decision: "blocked", reason: receipt.payload.reason, receipt }, true);
  }

  const verdict = authorize(ctx.policy, parsed.order, parsed.notionalUsd, now, state);
  if (verdict.kind === "block") {
    const receipt = emit("blocked", verdict.reason);
    return text({ decision: "blocked", reason: verdict.reason, receipt }, true);
  }
  if (verdict.kind === "needs_approval") {
    const receipt = emit("pending_approval", verdict.reason);
    return text({ decision: "pending_approval", reason: verdict.reason, receipt }, true);
  }

  const result = await ctx.upstream.callTool(name, args);
  if (!result.isError && parsed.notionalUsd != null) recordOrder(state, parsed.notionalUsd, now);
  const receipt = emit(result.isError ? "blocked" : "executed", result.isError ? "venue rejected the order" : undefined);
  return {
    ...result,
    content: [
      ...(Array.isArray(result.content) ? result.content : []),
      { type: "text", text: `covenant-guard receipt: ${JSON.stringify(receipt)}` },
    ],
  };
}

export function createGuardServer(ctx: GuardContext): Server {
  const state = newState();
  const server = new Server({ name: "covenant-robinhood-guard", version: "0.1.0" }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: await ctx.upstream.listTools() }));
  server.setRequestHandler(CallToolRequestSchema, async (req) =>
    guardedCall(ctx, state, req.params.name, (req.params.arguments ?? {}) as Record<string, unknown>),
  );
  return server;
}

export async function connectStdio(server: Server): Promise<void> {
  await server.connect(new StdioServerTransport());
}
