import { afterEach, describe, expect, it, vi } from "vitest";
import { internalSiteUrl } from "../internalSiteUrl";

function clearSiteEnv() {
  vi.stubEnv("COVENANT_INTERNAL_SITE_URL", "");
  vi.stubEnv("VERCEL_URL", "");
  vi.stubEnv("NEXT_PUBLIC_SITE_URL", "");
  vi.stubEnv("PORT", "");
}

afterEach(() => vi.unstubAllEnvs());

describe("internalSiteUrl", () => {
  it("uses loopback instead of request headers by default", () => {
    clearSiteEnv();
    expect(internalSiteUrl("/api/verify/abc").toString()).toBe(
      "http://127.0.0.1:3000/api/verify/abc",
    );
  });

  it("uses the explicitly configured internal origin", () => {
    clearSiteEnv();
    vi.stubEnv("COVENANT_INTERNAL_SITE_URL", "http://127.0.0.1:4000/base");
    expect(internalSiteUrl("/api/agents/abc").toString()).toBe(
      "http://127.0.0.1:4000/api/agents/abc",
    );
  });

  it("rejects credentials and non-HTTP schemes", () => {
    clearSiteEnv();
    vi.stubEnv("COVENANT_INTERNAL_SITE_URL", "https://user:pass@example.test");
    expect(() => internalSiteUrl("/api/verify/abc")).toThrow("invalid configured site URL");

    vi.stubEnv("COVENANT_INTERNAL_SITE_URL", "file:///tmp/covenant");
    expect(() => internalSiteUrl("/api/verify/abc")).toThrow("invalid configured site URL");
  });

  it("rejects protocol-relative and backslash paths", () => {
    clearSiteEnv();
    expect(() => internalSiteUrl("//example.test/api")).toThrow(
      "internal site path changed origin",
    );
    expect(() => internalSiteUrl("/\\example.test/api")).toThrow(
      "internal site path changed origin",
    );
  });
});
