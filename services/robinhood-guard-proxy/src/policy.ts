// The local trading policy, mirroring the Rust covenant-robinhood policy.rs so
// the proxy enforces the same rules the crate does. USD caps need an order
// value; when the value can't be determined from the tool-call args and a USD
// cap is set, the gate fails closed rather than let an unpriced order through.

export type Side = "buy" | "sell";

export interface TradingPolicy {
  venue?: string;
  caps?: { per_order_usd?: number; daily_notional_usd?: number };
  risk?: { daily_loss_stop_usd?: number };
  universe?: { allow?: string[]; deny?: string[]; sides?: Side[] };
  order_types?: string[];
  rate?: { max_orders_per_min?: number; cooldown_secs?: number };
  approvals?: { require_human_over_usd?: number };
}

export type Verdict =
  | { kind: "allow" }
  | { kind: "block"; reason: string }
  | { kind: "needs_approval"; reason: string };

export interface OrderIntent {
  symbol: string;
  side: Side;
  type?: string;
  quantity?: number;
  limitPrice?: number;
}

export interface PolicyState {
  dayNotionalUsd: number;
  dayLossUsd: number;
  orderTimes: number[];
  lastOrderAt?: number;
}

export const newState = (): PolicyState => ({ dayNotionalUsd: 0, dayLossUsd: 0, orderTimes: [] });

export function authorize(
  policy: TradingPolicy,
  order: OrderIntent,
  notionalUsd: number | null,
  now: number,
  st: PolicyState,
): Verdict {
  const u = policy.universe ?? {};
  if (u.deny?.includes(order.symbol)) return { kind: "block", reason: `symbol ${order.symbol} is on the deny list` };
  if (u.allow && !u.allow.includes(order.symbol))
    return { kind: "block", reason: `symbol ${order.symbol} is not on the allow list` };
  if (u.sides && !u.sides.includes(order.side))
    return { kind: "block", reason: `side ${order.side} is not permitted` };
  if (policy.order_types?.length && order.type && !policy.order_types.includes(order.type))
    return { kind: "block", reason: `order type ${order.type} is not permitted` };

  const stop = policy.risk?.daily_loss_stop_usd;
  if (stop != null && st.dayLossUsd >= stop)
    return { kind: "block", reason: `daily loss stop hit (${st.dayLossUsd.toFixed(2)} >= ${stop.toFixed(2)})` };

  const caps = policy.caps ?? {};
  const needsValue =
    caps.per_order_usd != null || caps.daily_notional_usd != null || policy.approvals?.require_human_over_usd != null;
  if (needsValue && notionalUsd == null)
    return { kind: "block", reason: "cannot determine order value in USD; refusing (fail-closed)" };

  if (caps.per_order_usd != null && notionalUsd != null && notionalUsd > caps.per_order_usd)
    return { kind: "block", reason: `per-order cap exceeded (${notionalUsd.toFixed(2)} > ${caps.per_order_usd.toFixed(2)})` };
  if (caps.daily_notional_usd != null && notionalUsd != null && st.dayNotionalUsd + notionalUsd > caps.daily_notional_usd)
    return {
      kind: "block",
      reason: `daily notional cap exceeded (${(st.dayNotionalUsd + notionalUsd).toFixed(2)} > ${caps.daily_notional_usd.toFixed(2)})`,
    };

  const maxPerMin = policy.rate?.max_orders_per_min;
  if (maxPerMin != null && st.orderTimes.filter((t) => now - t < 60).length >= maxPerMin)
    return { kind: "block", reason: `rate limit hit (${maxPerMin} orders/min)` };
  const cooldown = policy.rate?.cooldown_secs;
  if (cooldown != null && st.lastOrderAt != null && now - st.lastOrderAt < cooldown)
    return { kind: "block", reason: `cooldown active (${cooldown}s)` };

  const threshold = policy.approvals?.require_human_over_usd;
  if (threshold != null && notionalUsd != null && notionalUsd >= threshold)
    return {
      kind: "needs_approval",
      reason: `notional ${notionalUsd.toFixed(2)} at or above approval threshold ${threshold.toFixed(2)}`,
    };

  return { kind: "allow" };
}

export function recordOrder(st: PolicyState, notionalUsd: number, now: number): void {
  st.dayNotionalUsd += notionalUsd;
  st.orderTimes.push(now);
  st.orderTimes = st.orderTimes.filter((t) => now - t < 60);
  st.lastOrderAt = now;
}

// Robinhood's agentic MCP tool schema isn't public, so both the order-tool
// match and the field extraction are heuristic and overridable. Set
// COVENANT_GUARD_ORDER_TOOLS to the exact order-tool names once known.
export function isOrderTool(name: string, override?: string[]): boolean {
  if (override && override.length) return override.includes(name);
  const n = name.toLowerCase();
  if (/(get|list|read|status|history|quote|price|balance|position|search|watchlist)/.test(n)) return false;
  return /order|trade/.test(n) || n === "buy" || n === "sell";
}

const firstMatch = (args: Record<string, unknown>, re: RegExp): unknown => {
  for (const [k, v] of Object.entries(args)) if (re.test(k)) return v;
  return undefined;
};
const asNum = (v: unknown): number | undefined =>
  typeof v === "number" ? v : typeof v === "string" && v.trim() !== "" && !Number.isNaN(Number(v)) ? Number(v) : undefined;

export function extractOrder(args: Record<string, unknown>): { order: OrderIntent; notionalUsd: number | null } | null {
  const symbol = String(firstMatch(args, /symbol|ticker|instrument|^asset$|pair/i) ?? "").toUpperCase();
  if (!symbol) return null;
  const sideRaw = String(firstMatch(args, /side|action|direction/i) ?? "buy").toLowerCase();
  const side: Side = sideRaw.includes("sell") ? "sell" : "buy";
  const typeRaw = firstMatch(args, /(^|_)type$|order_type/i);
  const type = typeRaw != null ? String(typeRaw).toLowerCase() : undefined;
  const notionalUsd =
    asNum(firstMatch(args, /amount_usd|notional|dollars|usd_amount|dollar_amount/i)) ??
    asNum(firstMatch(args, /^amount$|^usd$|^dollars$/i)) ??
    null;
  const quantity = asNum(firstMatch(args, /quantity|qty|shares|units|^size$/i));
  const limitPrice = asNum(firstMatch(args, /limit_price|^price$/i));
  return { order: { symbol, side, type, quantity, limitPrice }, notionalUsd };
}
