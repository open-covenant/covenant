import { describe, expect, it } from "vitest";
import { canonicalize } from "../jcs.js";
import { decodeChallenge, pickAccept } from "../challenge.js";
import { buildReceipt, verifyReceipt, receiptDigest } from "../receipt.js";
import { withCovenantReceipts, type FetchLike } from "../decorator.js";

// The exact base64 BlockRun returns for a default gpt-4o-mini 402.
const LIVE_B64 =
  "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0cyI6W3sic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMiLCJhbW91bnQiOiIzMDAwIiwiYXNzZXQiOiIweDgzMzU4OWZDRDZlRGI2RTA4ZjRjN0MzMkQ0ZjcxYjU0YmRBMDI5MTMiLCJwYXlUbyI6IjB4ZTkwMzAwMTRGNURBZTIxN2QwQTE1MmYwMkEwNDM1NjdiMTZjMWFCZiIsIm1heFRpbWVvdXRTZWNvbmRzIjozMDAsImV4dHJhIjp7Im5hbWUiOiJVU0QgQ29pbiIsInZlcnNpb24iOiIyIn19XSwicmVzb3VyY2UiOnsidXJsIjoiaHR0cHM6Ly9ibG9ja3J1bi5haS9hcGkvdjEvY2hhdC9jb21wbGV0aW9ucyIsImRlc2NyaXB0aW9uIjoiR1BULTRvIE1pbmkgQVBJIGNhbGwgKH4xNyBpbnB1dCwgODE5MiBtYXggb3V0cHV0IHRva2VucykiLCJtaW1lVHlwZSI6ImFwcGxpY2F0aW9uL2pzb24ifX0=";

describe("jcs", () => {
  it("sorts keys and drops whitespace", () => {
    expect(canonicalize({ b: 1, a: 2 })).toBe('{"a":2,"b":1}');
  });
  it("is stable across key order", () => {
    expect(canonicalize({ x: 1, y: [3, 2] })).toBe(canonicalize({ y: [3, 2], x: 1 }));
  });
});

describe("challenge", () => {
  it("decodes the live header", () => {
    const c = decodeChallenge(LIVE_B64);
    const a = pickAccept(c, "eip155:8453")!;
    expect(a.amount).toBe("3000");
    expect(a.payTo).toBe("0xe9030014F5DAe217d0A152f02A043567b16c1aBf");
  });
  it("extracts from www-authenticate", () => {
    const c = decodeChallenge(`X402 requirements="${LIVE_B64}"`);
    expect(c.accepts).toHaveLength(1);
  });
});

describe("receipt", () => {
  const payment = {
    network: "eip155:8453",
    asset: "0x8335",
    amount: "3000",
    amountUsdc: 0.003,
    payTo: "0xe903",
    tx: "0xabc",
  };

  it("marks delivered when served matches requested", async () => {
    const r = await buildReceipt({
      endpoint: "/v1/chat/completions",
      request: { model: "gpt-4o-mini", messages: [] },
      response: { model: "gpt-4o-mini", choices: [] },
      payment,
    });
    expect(r.verdict).toBe("delivered");
  });

  it("marks substituted when the router swaps the model", async () => {
    const r = await buildReceipt({
      endpoint: "/x",
      request: { model: "gpt-4o" },
      response: { model: "gpt-4o-mini" },
      payment,
    });
    expect(r.verdict).toBe("substituted");
  });

  it("digest is stable across response key order", async () => {
    const a = await buildReceipt({
      endpoint: "/x",
      request: { model: "m" },
      response: { model: "m", id: "1", choices: [] },
      payment,
    });
    const b = await buildReceipt({
      endpoint: "/x",
      request: { model: "m" },
      response: { choices: [], id: "1", model: "m" },
      payment,
    });
    expect(await receiptDigest(a)).toBe(await receiptDigest(b));
  });

  it("verify accepts a clean receipt and its digest", async () => {
    const r = await buildReceipt({
      endpoint: "/x",
      request: { model: "m" },
      response: { model: "m" },
      payment,
    });
    const digest = await receiptDigest(r);
    const v = await verifyReceipt(r, digest);
    expect(v.valid).toBe(true);
    expect(v.digestMatches).toBe(true);
  });

  it("verify catches a tampered verdict", async () => {
    const r = await buildReceipt({
      endpoint: "/x",
      request: { model: "gpt-4o" },
      response: { model: "cheap" },
      payment,
    });
    r.verdict = "delivered";
    const v = await verifyReceipt(r);
    expect(v.verdictConsistent).toBe(false);
    expect(v.expectedVerdict).toBe("substituted");
  });
});

describe("decorator", () => {
  it("pairs a 402 with its paid retry into one receipt", async () => {
    const base: FetchLike = async (_input, init) => {
      const paid = typeof init?.body === "string" && init.headers
        ? new Headers(init.headers).has("x-payment")
        : false;
      if (!paid) {
        return new Response("", { status: 402, headers: { "x-payment-required": LIVE_B64 } });
      }
      return new Response(JSON.stringify({ model: "gpt-4o-mini", choices: [] }), {
        status: 200,
        headers: { "x-payment-receipt": "0xdeadbeef", "x-clawrouter-model": "gpt-4o-mini" },
      });
    };

    const receipts: import("../receipt.js").CallReceipt[] = [];
    const wrapped = withCovenantReceipts(base, {
      onReceipt: (r) => {
        receipts.push(r);
      },
      rethrowReceiptErrors: true,
    });

    const body = JSON.stringify({ model: "gpt-4o-mini", messages: [] });
    const url = "https://blockrun.ai/api/v1/chat/completions";
    // First call: 402 (stashes the challenge, no receipt).
    await wrapped(url, { method: "POST", body });
    // Paid retry: emits the receipt.
    await wrapped(url, { method: "POST", body, headers: { "x-payment": "sig" } });
    // Let the fire-and-forget receipt build settle.
    await new Promise((r) => setTimeout(r, 10));

    expect(receipts).toHaveLength(1);
    const r = receipts[0];
    expect(r.verdict).toBe("delivered");
    expect(r.payment.tx).toBe("0xdeadbeef");
    expect(r.payment.amount).toBe("3000");
    expect(r.payment.amountUsdc).toBeCloseTo(0.003);
    expect(r.endpoint).toBe("/api/v1/chat/completions");
  });
});
