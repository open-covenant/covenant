/**
 * The drop-in wedge: wrap a `fetch` so every BlockRun call emits a Covenant
 * receipt, without changing how the call is made or the money moves.
 *
 * BlockRun's own client does the 402 → pay → retry loop through some `fetch`;
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
  /** Errors thrown by onReceipt are swallowed by default; set to surface them. */
  rethrowReceiptErrors?: boolean;
}

const RECEIPT_HEADER = "x-payment-receipt";
const REQUIRED_HEADER = "x-payment-required";

/**
 * Wrap a fetch. Returns a fetch with identical behaviour that additionally
 * emits a receipt for each completed BlockRun exchange.
 */
export function withCovenantReceipts(baseFetch: FetchLike, opts: WrapOptions): FetchLike {
  // Challenge stash, keyed by request signature, so a 402 seen on one call can
  // be paired with its paid retry. Entries are short-lived and cleared on use.
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
          pending.set(key, decodeChallenge(header));
        } catch {
          /* not a decodable challenge; ignore */
        }
      }
      return res;
    }

    // Emit a receipt for the completed call. Clone so the caller still gets an
    // unread body.
    const clone = res.clone();
    void (async () => {
      try {
        const request = safeParse(bodyText);
        const response = await safeJson(clone);
        const routing = readRouting(res.headers);
        const tx = res.headers.get(RECEIPT_HEADER) ?? undefined;
        const challenge = pending.get(key);
        pending.delete(key);
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
      } catch (err) {
        if (opts.rethrowReceiptErrors) throw err;
      }
    })();

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
  if (init?.body && typeof init.body === "string") return init.body;
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
