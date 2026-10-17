import { describe, expect, it, vi } from "vitest";
import { COMPUTE_MAX_BYTES, COMPUTE_ORIGIN, COMPUTE_TIMEOUT_MS, computePageUrl, fetchComputePage } from "../_compute";

const html = (body = "<title>GPU price history</title>", headers: Record<string, string> = {}) =>
  new Response(body, { headers: { "content-type": "text/html; charset=utf-8", ...headers } });

describe("Compute source isolation", () => {
  it("constructs only fixed-origin page URLs, including root and eight nested segments", () => {
    expect(computePageUrl(["prices"])).toBe(`${COMPUTE_ORIGIN}/prices`);
    expect(computePageUrl(["index"])).toBe(`${COMPUTE_ORIGIN}/`);
    expect(computePageUrl(Array(8).fill("a"))).toBe(`${COMPUTE_ORIGIN}/a/a/a/a/a/a/a/a`);
    expect(computePageUrl(Array(9).fill("a"))).toBeNull();
  });

  it.each([[], ["."], [".."], ["prices", ".."], ["%2e%2e"], ["//evil.test"],
    ["https:", "evil.test"], ["prices?url=https://evil.test"], ["prices#x"], ["a/b"], ["a\\b"], [""]])(
    "rejects unsafe path %j before fetching", async (...segments) => {
      const fetcher = vi.fn();
      expect(computePageUrl(segments)).toBeNull();
      await expect(fetchComputePage(segments, fetcher)).rejects.toThrow("Invalid Compute card path");
      expect(fetcher).not.toHaveBeenCalled();
    },
  );

  it("fetches anonymously without redirects and uses an eight-second signal", async () => {
    const timeout = vi.spyOn(AbortSignal, "timeout");
    const fetcher = vi.fn(async () => html());
    const page = await fetchComputePage(["prices"], fetcher);
    expect(page).toMatchObject({ status: 200, host: "compute-api.opencovenant.org", path: "/prices" });
    expect(page.html).toContain("GPU price history");
    expect(fetcher).toHaveBeenCalledWith(`${COMPUTE_ORIGIN}/prices`, {
      redirect: "manual", credentials: "omit", signal: expect.any(AbortSignal),
      headers: { accept: "text/html", "user-agent": "Twitterbot/1.0 (Covenant card renderer)" },
    });
    expect(timeout).toHaveBeenCalledWith(COMPUTE_TIMEOUT_MS);
    timeout.mockRestore();
  });

  it.each([301, 302, 307, 308, 401, 403, 500])("refuses status %i without following it", async (status) => {
    const fetcher = vi.fn(async () => new Response("", { status, headers: { location: "https://evil.test/", "content-type": "text/html" } }));
    await expect(fetchComputePage(["prices"], fetcher)).rejects.toThrow("not public HTML");
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it.each([404, 410])("preserves missing-page status %i", async (status) => {
    expect(await fetchComputePage(["missing"], async () => new Response("", { status }))).toMatchObject({ status, html: "" });
  });

  it.each([
    { "content-type": "application/json" },
    { "content-type": "text/plain" },
    { "cache-control": "max-age=60, private" },
    { "cache-control": "no-store" },
    { "set-cookie": "session=anything" },
  ] as Record<string, string>[])("refuses non-HTML or private responses %j", async (headers) => {
    await expect(fetchComputePage(["prices"], async () => html("private", headers))).rejects.toThrow("not public HTML");
  });

  it("refuses an unexpectedly followed redirect", async () => {
    const response = html();
    Object.defineProperty(response, "redirected", { value: true });
    await expect(fetchComputePage(["prices"], async () => response)).rejects.toThrow("not public HTML");
  });

  it("rejects oversized declared bodies before reading them", async () => {
    await expect(fetchComputePage(["prices"], async () => html("", { "content-length": `${COMPUTE_MAX_BYTES + 1}` })))
      .rejects.toThrow("too large");
  });

  it("bounds streamed bodies even when the content length is absent or false", async () => {
    let cancelled = false;
    const response = new Response(new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array(COMPUTE_MAX_BYTES));
        controller.enqueue(new Uint8Array(1));
      },
      cancel() { cancelled = true; },
    }), { headers: { "content-type": "text/html", "content-length": "10" } });
    await expect(fetchComputePage(["prices"], async () => response)).rejects.toThrow("too large");
    expect(cancelled).toBe(true);
  });

  it("propagates timeout/network failures instead of producing a card", async () => {
    await expect(fetchComputePage(["prices"], async () => { throw new DOMException("Timed out", "TimeoutError"); }))
      .rejects.toThrow("Timed out");
  });
});
