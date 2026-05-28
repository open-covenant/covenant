import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

const ENV_KEYS = [
  "CODER_IP_MAX_PER_IP",
  "CODER_IP_REFILL_MS",
  "TRUSTED_PROXY_HOPS",
] as const;

const savedEnv: Partial<Record<(typeof ENV_KEYS)[number], string | undefined>> = {};

describe("config env validation", () => {
  beforeEach(() => {
    for (const k of ENV_KEYS) savedEnv[k] = process.env[k];
  });

  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (savedEnv[k] === undefined) delete process.env[k];
      else process.env[k] = savedEnv[k];
    }
  });

  // Each test re-imports config so module-load-time validation runs
  // against the current env. Vitest caches imports per worker, so we
  // reset the module registry between tests.
  async function loadConfig(): Promise<typeof import("../src/config.js").config> {
    vi.resetModules();
    const { config } = await import("../src/config.js");
    return config;
  }

  it("accepts an explicit zero opt-out", async () => {
    process.env["CODER_IP_MAX_PER_IP"] = "0";
    const c = await loadConfig();
    expect(c.ipMaxPerIp).toBe(0);
  });

  it("refuses a typo that would silently disable the per-IP gate", async () => {
    // `Number('true')` is NaN, and `NaN > 0` is false — the previous
    // code would silently disable the gate. The validator must reject
    // at boot so the operator notices the typo.
    process.env["CODER_IP_MAX_PER_IP"] = "true";
    await expect(loadConfig()).rejects.toThrow(/CODER_IP_MAX_PER_IP/);
  });

  it("refuses a negative TRUSTED_PROXY_HOPS instead of silently defaulting", async () => {
    process.env["TRUSTED_PROXY_HOPS"] = "-1";
    await expect(loadConfig()).rejects.toThrow(/TRUSTED_PROXY_HOPS/);
  });

  it("refuses a fractional CODER_IP_REFILL_MS to keep ms math integer-clean", async () => {
    process.env["CODER_IP_REFILL_MS"] = "1.5";
    await expect(loadConfig()).rejects.toThrow(/CODER_IP_REFILL_MS/);
  });

  it("treats an empty env value as 'unset' and uses the default", async () => {
    process.env["CODER_IP_MAX_PER_IP"] = "";
    const c = await loadConfig();
    expect(c.ipMaxPerIp).toBe(1);
  });
});
