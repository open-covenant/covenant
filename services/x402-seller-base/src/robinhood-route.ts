// Paid Robinhood handlers, extracted from server.ts so tests can exercise them
// without the payment middleware. Reached only after a verified payment; both
// proxy to covenantd, which runs the capability gate + policy and signs the
// receipt in the sidecar. On any daemon/transport failure we return >= 400 so
// the x402 middleware cancels settlement and the buyer is not charged.
import type { Request, Response } from "express";

const DAEMON_URL = (process.env.COVENANT_HTTP_URL ?? "http://127.0.0.1:8421").replace(/\/+$/, "");

async function daemonCall(name: string, args: unknown): Promise<{ status: number; body: unknown }> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const token = process.env.COVENANT_AUTH_TOKEN?.trim();
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(`${DAEMON_URL}/tools/call`, {
    method: "POST",
    headers,
    body: JSON.stringify({ name, arguments: args }),
  });
  const text = await r.text();
  let body: unknown = null;
  if (text.trim()) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }
  return { status: r.status, body };
}

// A governed decision (executed or blocked) is a signed, on-chain-anchored
// receipt: the product the buyer paid for. Charging only on `executed` (4xx to
// cancel settlement on a block) is a straightforward refinement once the
// charging policy is settled.
export function makeGovernedOrderHandler() {
  return async (req: Request, res: Response): Promise<void> => {
    const b = (req.body ?? {}) as Record<string, unknown>;
    if (typeof b.symbol !== "string" || (b.side !== "buy" && b.side !== "sell") || typeof b.quantity !== "number") {
      res.status(400).json({ error: "symbol, side (buy|sell), and quantity are required" });
      return;
    }
    const args: Record<string, unknown> = { symbol: b.symbol, side: b.side, quantity: b.quantity };
    if (typeof b.type === "string") args.type = b.type;
    if (typeof b.limit_price === "number") args.limit_price = b.limit_price;
    try {
      const { status, body } = await daemonCall("robinhood.place_order", args);
      if (status < 200 || status >= 300) {
        res.status(502).json({ error: "covenantd call failed", detail: body });
        return;
      }
      res.json(body);
    } catch (e) {
      res.status(502).json({ error: `covenantd unreachable: ${e instanceof Error ? e.message : String(e)}` });
    }
  };
}

export function makeReputationHandler() {
  return async (req: Request, res: Response): Promise<void> => {
    try {
      const { status, body } = await daemonCall("robinhood.reputation", { agent: req.params.agent });
      if (status < 200 || status >= 300) {
        res.status(502).json({ error: "covenantd call failed", detail: body });
        return;
      }
      res.json(body);
    } catch (e) {
      res.status(502).json({ error: `covenantd unreachable: ${e instanceof Error ? e.message : String(e)}` });
    }
  };
}
