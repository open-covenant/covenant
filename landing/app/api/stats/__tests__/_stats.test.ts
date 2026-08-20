import { afterEach, describe, expect, it, vi } from "vitest";
import { ghHeaders, parseMetrics } from "../_stats";

describe("parseMetrics", () => {
  it("extracts the test, live, and crate counts from the status line", () => {
    const md =
      "28 Rust crates, ~237k lines, 3197 source-discovered Rust tests including 464 live boundary tests.";
    expect(parseMetrics(md)).toEqual({ tests: "3197", live: "464", crates: "28" });
  });

  it("keeps thousands separators in the captured counts", () => {
    const md =
      "9 Rust crates, 1,234 source-discovered Rust tests including 1,000 live boundary tests.";
    expect(parseMetrics(md)).toMatchObject({ tests: "1,234", live: "1,000" });
  });

  it("falls back to null for any metric the README does not state", () => {
    expect(parseMetrics("no metrics stated in this prose")).toEqual({
      tests: null,
      live: null,
      crates: null,
    });
  });
});

describe("ghHeaders", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("omits the authorization header when no token is configured", () => {
    vi.stubEnv("GITHUB_TOKEN", "");
    vi.stubEnv("GH_TOKEN", "");
    const h = ghHeaders();
    expect(h["user-agent"]).toBe("covenant-hud");
    expect(h.accept).toBe("application/vnd.github+json");
    expect("authorization" in h).toBe(false);
  });

  it("attaches a bearer authorization header from GITHUB_TOKEN", () => {
    vi.stubEnv("GITHUB_TOKEN", "tok-primary");
    vi.stubEnv("GH_TOKEN", "");
    expect(ghHeaders()).toMatchObject({ authorization: "Bearer tok-primary" });
  });

  it("falls back to GH_TOKEN, and prefers GITHUB_TOKEN when both are set", () => {
    vi.stubEnv("GITHUB_TOKEN", "");
    vi.stubEnv("GH_TOKEN", "tok-fallback");
    expect(ghHeaders()).toMatchObject({ authorization: "Bearer tok-fallback" });

    vi.stubEnv("GITHUB_TOKEN", "tok-primary");
    vi.stubEnv("GH_TOKEN", "tok-fallback");
    expect(ghHeaders()).toMatchObject({ authorization: "Bearer tok-primary" });
  });
});
