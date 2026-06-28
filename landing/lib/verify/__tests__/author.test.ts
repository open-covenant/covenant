import { afterEach, describe, expect, it, vi } from "vitest";
import { redactAuthor } from "../author";

// redactAuthor is the identity-protection gate on the public witness commit
// header (app/api/verify/[sha]): if an author name OR email carries a runtime
// leak token, both fields are replaced with a public legacy label instead of
// being rendered. The leak tokens are derived at import time, so the
// single-field redaction arms are exercised with a stubbed env and a fresh
// module so the fail-closed OR cannot silently weaken to an AND.
describe("redactAuthor", () => {
  it("passes a clean name and email through unchanged", () => {
    expect(redactAuthor("Ada Lovelace", "ada@example.com")).toEqual({
      display: "Ada Lovelace",
      email: "ada@example.com",
    });
  });

  it("does not swap the name and email fields on passthrough", () => {
    const out = redactAuthor("Grace Hopper", "grace@example.com");
    expect(out.display).toBe("Grace Hopper");
    expect(out.email).toBe("grace@example.com");
  });
});

describe("redactAuthor leak-token substitution", () => {
  const TOKEN = "covredactsentinel";
  const LEGACY = { display: "Covenant Legacy", email: "legacy@opencovenant.org" };

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  const fresh = async () => {
    vi.stubEnv("USER", TOKEN);
    vi.resetModules();
    return (await import("../author")).redactAuthor;
  };

  it("substitutes the legacy label when only the name carries a leak token", async () => {
    const redact = await fresh();
    expect(redact(`authored by ${TOKEN}`, "clean@example.com")).toEqual(LEGACY);
  });

  it("substitutes the legacy label when only the email carries a leak token", async () => {
    const redact = await fresh();
    expect(redact("Clean Name", `${TOKEN}@host.internal`)).toEqual(LEGACY);
  });

  it("substitutes the legacy label when both fields carry a leak token", async () => {
    const redact = await fresh();
    expect(redact(`${TOKEN} one`, `${TOKEN}@two`)).toEqual(LEGACY);
  });

  it("still passes a clean pair through while the leak token is active", async () => {
    const redact = await fresh();
    expect(redact("Ordinary Author", "ordinary@example.com")).toEqual({
      display: "Ordinary Author",
      email: "ordinary@example.com",
    });
  });
});
