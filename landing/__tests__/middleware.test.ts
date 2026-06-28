import { describe, expect, it } from "vitest";
import { NextRequest } from "next/server";
import { middleware } from "@/middleware";

// landing/middleware.ts routes the marketing site by host. On the docs.*
// subdomain it 308-redirects /docs-prefixed paths back to their clean canonical
// form and rewrites everything else into the /docs tree; on the apex it
// 308-redirects /docs and /docs/* up to docs.<apex>; all other traffic passes
// through. A NextResponse signals its decision through internal headers —
// rewrites set x-middleware-rewrite, pass-throughs set x-middleware-next, and
// redirects set Location with a 308 status — so each arm asserts the header
// that proves which branch ran plus the exact target it built. They pin the
// /docs prefix strip, the query preservation, the rewrite of the root and of
// deeper paths into the /docs tree, the apex docs-host construction with its
// www. strip, and the forwarded-proto preference against a silent regression in
// the public docs/marketing routing. (The empty-path-to-/ fallbacks are not
// asserted: the URL constructor already normalizes them, so they carry no
// independently observable behavior.)

function run(host: string, url: string, headers: Record<string, string> = {}) {
  return middleware(new NextRequest(new URL(url), { headers: { host, ...headers } }));
}

const rewritePath = (res: Response) => {
  const target = res.headers.get("x-middleware-rewrite");
  return target ? new URL(target).pathname : null;
};

describe("middleware on the docs subdomain", () => {
  it("rewrites the root into /docs", () => {
    const res = run("docs.example.com", "https://docs.example.com/");
    expect(rewritePath(res)).toBe("/docs");
    expect(res.headers.get("x-middleware-next")).toBeNull();
  });

  it("rewrites a deeper path under /docs", () => {
    const res = run("docs.example.com", "https://docs.example.com/cli");
    expect(rewritePath(res)).toBe("/docs/cli");
  });

  it("308-redirects a /docs-prefixed path to its clean form, preserving the query", () => {
    const res = run("docs.example.com", "https://docs.example.com/docs/cli?q=1");
    expect(res.status).toBe(308);
    expect(res.headers.get("location")).toBe("https://docs.example.com/cli?q=1");
    expect(res.headers.get("x-middleware-rewrite")).toBeNull();
  });

  it("collapses a bare /docs to the root", () => {
    const res = run("docs.example.com", "https://docs.example.com/docs");
    expect(res.status).toBe(308);
    expect(res.headers.get("location")).toBe("https://docs.example.com/");
  });

  it("prefers x-forwarded-proto over the request protocol", () => {
    const res = run("docs.example.com", "http://docs.example.com/docs/x", {
      "x-forwarded-proto": "https",
    });
    expect(res.headers.get("location")).toBe("https://docs.example.com/x");
  });
});

describe("middleware on the apex", () => {
  it("308-redirects /docs/* up to docs.<apex>, preserving the query", () => {
    const res = run("example.com", "https://example.com/docs/foo?q=1");
    expect(res.status).toBe(308);
    expect(res.headers.get("location")).toBe("https://docs.example.com/foo?q=1");
  });

  it("strips a leading www. when building the docs host", () => {
    const res = run("www.example.com", "https://www.example.com/docs");
    expect(res.status).toBe(308);
    expect(res.headers.get("location")).toBe("https://docs.example.com/");
  });

  it("falls back to the request protocol when x-forwarded-proto is absent", () => {
    const res = run("example.com", "http://example.com/docs");
    expect(res.headers.get("location")).toBe("http://docs.example.com/");
  });

  it("passes a non-docs path through untouched", () => {
    const res = run("example.com", "https://example.com/positions");
    expect(res.headers.get("x-middleware-next")).toBe("1");
    expect(res.headers.get("x-middleware-rewrite")).toBeNull();
    expect(res.headers.get("location")).toBeNull();
  });
});
