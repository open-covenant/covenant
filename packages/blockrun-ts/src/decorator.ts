/**
 * The drop-in wedge: wrap a `fetch` so every settled BlockRun call emits a
 * Covenant receipt, without changing how the call is made or the money moves.
 *
 * BlockRun's own client does the 402 -> pay -> retry loop through some `fetch`;
 * we sit around that fetch, remember the challenge from the 402, and pair it
 * with the settled retry (whose `x-payment-receipt` header carries the tx) to
 * build a complete receipt. The response is passed through untouched.
 */
import { acceptToPayment, decodeChallenge, pickAccept } from "./challenge.js";
import { buildReceipt, type CallReceipt, type PaymentInfo, type RoutingClaim } from "./receipt.js";

export type FetchLike = (
  input: string | URL | Request,
  init?: RequestInit,
) => Promise<Response>;

export type OnReceipt = (receipt: CallReceipt) => void | Promise<void>;

export interface WrapOptions {
  /** Called once per settled (or free) BlockRun call with its receipt. */
  onReceipt: OnReceipt;
  /**
   * Notified if building or delivering a receipt throws. Receipt work never
   * affects the underlying call, so errors are reported here rather than raised
   * into the caller. Without this they are swallowed.
   */
  onError?: (err: unknown) => void;
}

const RECEIPT_HEADER = "x-payment-receipt";
const REQUIRED_HEADER = "x-payment-required";

// Cap on outstanding 402 challenges awaiting their paid retry. A 402 that is
// never paired (payment fails, caller aborts) would otherwise leak one entry
// forever; past this many we drop the oldest.
const MAX_PENDING = 1024;

/**
 * Wrap a fetch. Returns a fetch with identical behaviour that additionally
 * emits a receipt for each completed BlockRun exchange.
 */
export function withCovenantReceipts(baseFetch: FetchLike, opts: WrapOptions): FetchLike {
  const pending = new Map<string, ReturnType<typeof decodeChallenge>>();

  return async (input, init) => {
    const url = requestUrl(input);
    const method = (init?.method ?? requestMethod(input) ?? "GET").toUpperCase();
    const bodyText = await readRequestBody(input, init);
    const key = `${method} ${url} ${bodyText ?? ""}`;

    const res = await baseFetch(input, init);

    // 402: remember the challenge, do not emit a receipt yet.
    if (res.status === 402) {
      const header = res.headers.get(REQUIRED_HEADER) ?? res.headers.get("www-authenticate");
      if (header) {
        try {
          if (pending.size >= MAX_PENDING) {
            pending.delete(pending.keys().next().value as string);
          }
          pending.set(key, decodeChallenge(header));
        } catch (err) {
          opts.onError?.(err);
        }
      }
      return res;
    }

    // Only a delivered (2xx) call gets a receipt. A challenge left unpaired by a
    // failed retry is cleaned up here rather than left to leak.
    const challenge = pending.get(key);
    pending.delete(key);
    if (!res.ok) return res;

    // Clone so the caller still gets an unread body.
    const clone = res.clone();
    void (async () => {
      const request = safeParse(bodyText);
      const response = await safeJson(clone);
      const routing = readRouting(res.headers);
      const tx = res.headers.get(RECEIPT_HEADER) ?? undefined;
      const accept = challenge ? pickAccept(challenge) : undefined;
      const payment: PaymentInfo = accept
        ? acceptToPayment(accept, tx)
        : { network: "", asset: "", amount: "0", amountUsdc: 0, payTo: "", tx };
      const receipt = await buildReceipt({
        endpoint: endpointOf(url),
        request,
        response,
        payment,
        routing,
      });
      await opts.onReceipt(receipt);
    })().catch((err) => opts.onError?.(err));

    return res;
  };
}

function readRouting(headers: Headers): RoutingClaim {
  const first = (...names: string[]): string | undefined => {
    for (const n of names) {
      const v = headers.get(n);
      if (v) return v;
    }
    return undefined;
  };
  const savings = first("x-clawrouter-savings", "x-blockrun-savings");
  const routing: RoutingClaim = {
    profile: first("x-clawrouter-profile", "x-blockrun-profile"),
    model: first("x-clawrouter-model", "x-blockrun-model", "x-model"),
  };
  if (savings) {
    const n = Number.parseFloat(savings.replace(/%$/, "").trim());
    if (Number.isFinite(n)) routing.savingsPct = n;
  }
  return routing;
}

function requestUrl(input: string | URL | Request): string {
  if (typeof input === "string") return input;
  if (input instanceof URL) return input.toString();
  return input.url;
}

function requestMethod(input: string | URL | Request): string | undefined {
  return input instanceof Request ? input.method : undefined;
}

async function readRequestBody(
  input: string | URL | Request,
  init?: RequestInit,
): Promise<string | undefined> {
  const body = init?.body;
  if (typeof body === "string") return body;
  if (body instanceof Uint8Array) return new TextDecoder().decode(body);
  if (body instanceof ArrayBuffer) return new TextDecoder().decode(new Uint8Array(body));
  if (input instanceof Request) {
    try {
      return await input.clone().text();
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function endpointOf(url: string): string {
  try {
    return new URL(url).pathname;
  } catch {
    return url;
  }
}

function safeParse(text: string | undefined): unknown {
  if (!text) return {};
  try {
    return JSON.parse(text);
  } catch {
    return {};
  }
}

async function safeJson(res: Response): Promise<unknown> {
  try {
    return await res.json();
  } catch {
    return {};
  }
}
