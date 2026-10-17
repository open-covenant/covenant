import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { proxyPriceData } from "../_proxy";

const json = (body = '{"points":[]}', headers: Record<string, string> = {}) => new Response(body, { headers: { "content-type": "application/json", ...headers } });
beforeEach(() => vi.spyOn(console, "error").mockImplementation(() => {}));
afterEach(() => vi.restoreAllMocks());

describe("public price-data proxy", () => {
  it("forwards bounded query parameters only to the fixed history origin without credentials", async () => {
    const fetcher = vi.fn(async () => json());
    const response = await proxyPriceData("/prices/history", new URLSearchParams("since_ms=123&until_ms=456"), fetcher);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ points: [] });
    expect(fetcher).toHaveBeenCalledWith("https://compute-api.opencovenant.org/prices/history?since_ms=123&until_ms=456", expect.objectContaining({
      redirect: "manual", credentials: "omit", headers: { accept: "application/json" }, signal: expect.any(AbortSignal),
    }));
  });

  it("permits only UUID scoped-proof paths", async () => {
    const fetcher = vi.fn(async () => json());
    const proof = "/prices/proofs/12345678-1234-1234-1234-123456789abc";
    expect((await proxyPriceData(proof, new URLSearchParams(), fetcher)).status).toBe(200);
    expect(fetcher).toHaveBeenCalledWith(`https://compute-api.opencovenant.org${proof}`, expect.anything());
    fetcher.mockClear();
    for (const path of ["/prices/proofs/../history", "//evil.test/", "/leases", "/prices/proofs/not-a-uuid"]) {
      expect((await proxyPriceData(path, new URLSearchParams(), fetcher)).status).toBe(404);
    }
    expect(fetcher).not.toHaveBeenCalled();
  });

  it.each(["url=https://evil.test", "since_ms=1&since_ms=2", "since_ms=-1", "until_ms=18446744073709551616", "since_ms=abc"])(
    "rejects unsupported query %s without fetching", async (query) => {
      const fetcher = vi.fn();
      expect((await proxyPriceData("/prices/history", new URLSearchParams(query), fetcher)).status).toBe(400);
      expect(fetcher).not.toHaveBeenCalled();
    },
  );

  it("does not follow redirects or expose malformed JSON", async () => {
    for (const response of [new Response("", { status: 302, headers: { location: "https://evil.test/" } }), json("not JSON"), json("{}", { "set-cookie": "secret=1" })]) {
      const result = await proxyPriceData("/prices/history", new URLSearchParams(), async () => response);
      expect(result.status).toBe(503);
      expect(result.headers.get("cache-control")).toBe("no-store");
    }
  });

  it("preserves not-found and rate-limited statuses without forwarding response data", async () => {
    for (const status of [404, 429]) {
      const result = await proxyPriceData("/prices/history", new URLSearchParams(), async () => new Response("upstream details", { status }));
      expect(result.status).toBe(status);
      expect(await result.text()).not.toContain("upstream details");
    }
  });
});
