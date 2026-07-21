import { describe, expect, it } from "vitest";
import type { CallToolResult, Tool } from "@modelcontextprotocol/sdk/types.js";

import { guardedCall, type GuardContext, type Upstream } from "../proxy";
import { newState, type TradingPolicy } from "../policy";
import { GuardAttestor, verifyDecision, type SignedDecision } from "../receipt";

class MockUpstream implements Upstream {
  calls: { name: string; args: Record<string, unknown> }[] = [];
  constructor(
    private toolsList: Tool[] = [],
    private result: CallToolResult = { content: [{ type: "text", text: "venue ok" }] },
  ) {}
  async listTools() {
    return this.toolsList;
  }
  async callTool(name: string, args: Record<string, unknown>) {
    this.calls.push({ name, args });
    return this.result;
  }
  async close() {}
}

const POLICY: TradingPolicy = {
  venue: "robinhood",
  caps: { per_order_usd: 500, daily_notional_usd: 2000 },
  universe: { allow: ["BTC-USD", "ETH-USD"], sides: ["buy"] },
  approvals: { require_human_over_usd: 400 },
};

function ctx(upstream: Upstream): GuardContext {
  return { upstream, policy: POLICY, attestor: GuardAttestor.generate(), now: () => 1_700_000_000 };
}

// The receipt is appended to results (allow) or nested in the error body (block).
function receiptFrom(result: CallToolResult): SignedDecision {
  const texts = (result.content ?? []).map((c) => (c as { text?: string }).text ?? "");
  const tagged = texts.find((t) => t.startsWith("covenant-guard receipt: "));
  if (tagged) return JSON.parse(tagged.replace("covenant-guard receipt: ", ""));
  return JSON.parse(texts[0] ?? "{}").receipt;
}

describe("guarded proxy", () => {
  it("passes non-order tools straight through", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "get_portfolio", { account: "x" });
    expect(res.isError).toBeFalsy();
    expect(up.calls).toEqual([{ name: "get_portfolio", args: { account: "x" } }]);
  });

  it("forwards a within-policy order and appends a valid receipt", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "place_order", { symbol: "BTC-USD", side: "buy", amount_usd: 60 });
    expect(res.isError).toBeFalsy();
    expect(up.calls).toHaveLength(1);
    const r = receiptFrom(res);
    expect(r.payload.decision).toBe("executed");
    expect(verifyDecision(r)).toBe(true);
  });

  it("blocks an over-cap order before it reaches the venue", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "place_order", { symbol: "BTC-USD", side: "buy", amount_usd: 1200 });
    expect(res.isError).toBe(true);
    expect(up.calls).toHaveLength(0);
    const r = receiptFrom(res);
    expect(r.payload.decision).toBe("blocked");
    expect(r.payload.reason).toContain("per-order");
    expect(verifyDecision(r)).toBe(true);
  });

  it("blocks a symbol outside the allow list", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "place_order", { symbol: "DOGE-USD", side: "buy", amount_usd: 50 });
    expect(res.isError).toBe(true);
    expect(up.calls).toHaveLength(0);
    expect(receiptFrom(res).payload.reason).toContain("allow list");
  });

  it("holds an over-threshold order for approval", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "place_order", { symbol: "BTC-USD", side: "buy", amount_usd: 450 });
    expect(res.isError).toBe(true);
    expect(up.calls).toHaveLength(0);
    expect(receiptFrom(res).payload.decision).toBe("pending_approval");
  });

  it("fails closed when it can't parse an order and a cap is set", async () => {
    const up = new MockUpstream();
    const res = await guardedCall(ctx(up), newState(), "submit_order", { side: "buy", amount_usd: 50 });
    expect(res.isError).toBe(true);
    expect(up.calls).toHaveLength(0);
    expect(receiptFrom(res).payload.reason).toContain("fail-closed");
  });
});
