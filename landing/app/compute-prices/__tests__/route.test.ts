import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GET } from "../route";
import { fetchComputePage } from "../../og/_compute";
import { readCardData } from "../../og/_page";

vi.mock("../../og/_compute", () => ({ fetchComputePage: vi.fn() }));
const source = (status = 200) => ({
  status, host: "compute-api.opencovenant.org", path: "/prices/page",
  headers: new Headers({ "content-security-policy": "default-src 'none'; script-src 'unsafe-inline'", "referrer-policy": "no-referrer" }),
  html: '<!doctype html><html><head><title>Source title</title><link rel="canonical" href="https://old.example/"><meta property="og:image" content="old.png"><style>.price{color:white}</style></head><body><h1>What GPU time costs.</h1><script>fetch("/prices/history")</script></body></html>',
});

beforeEach(() => { vi.resetAllMocks(); vi.spyOn(console, "error").mockImplementation(() => {}); });
afterEach(() => vi.restoreAllMocks());

describe("main-site price page", () => {
  it("serves the source UI with one canonical sharing image and preserved CSP", async () => {
    vi.mocked(fetchComputePage).mockResolvedValue(source());
    const response = await GET();
    const body = await response.text();
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(response.headers.get("content-security-policy")).toBe(source().headers.get("content-security-policy"));
    expect(body).toContain('<script>fetch("/prices/history")</script>');
    expect(body).toContain('<style>.price{color:white}</style>');
    expect(body.match(/property="og:image"/g)).toHaveLength(1);
    expect(body.match(/rel="canonical"/g)).toHaveLength(1);
    expect(body).toContain('name="twitter:image" content="https://opencovenant.org/og/compute-prices.png"');
    expect(readCardData(body)).toMatchObject({ title: "GPU price history", section: "Compute", url: "https://opencovenant.org/compute-prices" });
    expect(fetchComputePage).toHaveBeenCalledWith(["prices", "page"]);
  });

  it("supports main-site-first rollout by falling back only for a missing source route", async () => {
    vi.mocked(fetchComputePage).mockResolvedValueOnce(source(404)).mockResolvedValueOnce(source());
    expect((await GET()).status).toBe(200);
    expect(fetchComputePage).toHaveBeenNthCalledWith(1, ["prices", "page"]);
    expect(fetchComputePage).toHaveBeenNthCalledWith(2, ["prices"]);
  });

  it.each([410, 500])("does not hide source status %i behind fallback", async (status) => {
    vi.mocked(fetchComputePage).mockResolvedValue(source(status));
    const response = await GET();
    expect(response.status).toBe(503);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(fetchComputePage).toHaveBeenCalledTimes(1);
  });

  it("fails unavailable when source fetch times out", async () => {
    vi.mocked(fetchComputePage).mockRejectedValue(new Error("Timeout"));
    expect((await GET()).status).toBe(503);
    expect(fetchComputePage).toHaveBeenCalledTimes(1);
  });
});
