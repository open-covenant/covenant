import { afterEach, describe, expect, it, vi } from "vitest";
import { clean } from "../agentStream.mjs";

// clean() is the identity-protection scrubber the public witness surface relies
// on (app/api/verify redactAuthor, app/agent-stream): it nulls out a line that
// carries any runtime-derived identity token and rewrites /Users/<name> home
// paths to ~. Its leak tokens are derived at import time, so the token-drop arm
// is exercised with a stubbed env and a fresh module.
describe("clean", () => {
  it("returns null for nullish input", () => {
    expect(clean(null)).toBeNull();
    expect(clean(undefined)).toBeNull();
  });

  it("rewrites a home path to ~", () => {
    expect(clean("see /Users/alice/secret.txt")).toBe("see ~/secret.txt");
  });

  it("rewrites every home path, not just the first", () => {
    expect(clean("/Users/a/x and /Users/b/y")).toBe("~/x and ~/y");
  });

  it("strips a single trailing carriage return", () => {
    expect(clean("a commit subject\r")).toBe("a commit subject");
  });

  it("passes a clean line through unchanged", () => {
    expect(clean("a routine parser refactor")).toBe("a routine parser refactor");
  });
});

describe("clean leak-token drop", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  it("drops a line containing a runtime identity token", async () => {
    vi.stubEnv("USER", "covsentineluser");
    vi.resetModules();
    const { clean: freshClean } = await import("../agentStream.mjs");
    expect(freshClean("authored by covsentineluser")).toBeNull();
    expect(freshClean("an ordinary commit subject")).toBe("an ordinary commit subject");
  });
});
