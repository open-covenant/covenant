import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchCvntSolPrice } from "../price";

const okQuote = (body: unknown) => ({ ok: true, json: async () => body });

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("fetchCvntSolPrice", () => {
  it("returns SOL-per-CVNT from a valid quote and floors fetchedAt to whole seconds", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_700_000_000_789);
    const fetchMock = vi
      .fn()
      .mockResolvedValue(okQuote({ outAmount: "7000000", inAmount: "2000000" }));
    vi.stubGlobal("fetch", fetchMock);

    expect(await fetchCvntSolPrice()).toEqual({
      solPerCvnt: 0.0035,
      fetchedAt: 1_700_000_000,
    });

    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toContain("inputMint=2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump");
    expect(url).toContain("outputMint=So11111111111111111111111111111111111111112");
    expect(url).toContain("amount=1000000");
    expect(url).toContain("slippageBps=300");
    expect(init).toEqual({ cache: "no-store" });
  });

  it("fails closed on a non-ok HTTP response even when the body is a valid quote", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: false,
      json: async () => ({ outAmount: "7000000", inAmount: "2000000" }),
    }));
    expect(await fetchCvntSolPrice()).toBeNull();
  });

  it.each([
    ["outAmount is missing", { inAmount: "2000000" }],
    ["inAmount is missing", { outAmount: "7000000" }],
    ["outAmount is non-numeric", { outAmount: "abc", inAmount: "2000000" }],
    ["inAmount is non-numeric", { outAmount: "7000000", inAmount: "abc" }],
    ["outAmount is zero", { outAmount: "0", inAmount: "2000000" }],
    ["inAmount is zero", { outAmount: "7000000", inAmount: "0" }],
    ["outAmount is negative", { outAmount: "-7000000", inAmount: "2000000" }],
    ["inAmount is negative", { outAmount: "7000000", inAmount: "-2000000" }],
  ])("fails closed when %s", async (_label, body) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(okQuote(body)));
    expect(await fetchCvntSolPrice()).toBeNull();
  });

  // A denormal-tiny amount passes the > 0 check but underflows the unit conversion,
  // so the computed-price guard is the only thing standing between it and a bogus number.
  it("fails closed when the computed price is non-finite (inAmount underflow)", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(okQuote({ outAmount: "1000000", inAmount: "5e-324" })));
    expect(await fetchCvntSolPrice()).toBeNull();
  });

  it("fails closed when the computed price underflows to zero (outAmount underflow)", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(okQuote({ outAmount: "5e-324", inAmount: "1000000" })));
    expect(await fetchCvntSolPrice()).toBeNull();
  });

  it("fails closed when fetch rejects", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("network down")));
    expect(await fetchCvntSolPrice()).toBeNull();
  });

  it("fails closed when the response body is not valid JSON", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      json: async () => {
        throw new SyntaxError("unexpected token");
      },
    }));
    expect(await fetchCvntSolPrice()).toBeNull();
  });
});
