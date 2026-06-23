import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// loadConfig reads process.env at call time and validates it. Each test sets a
// valid base env, mutates one variable, and re-imports the module (vi.resetModules
// defeats per-worker import caching) so the validation runs against that env.
const ENV_KEYS = [
  "HATCHER_CONNECTOR_TOKEN",
  "COVENANT_CONNECTOR_TOKEN",
  "COVENANT_OPERATOR_TOKEN",
  "COVENANT_DAEMON_URL",
  "HATCHER_MESH_URL",
  "HATCHER_PAIRING_CODE",
  "PORT",
  "HATCHER_CONNECTOR_PORT",
  "HATCHER_MAX_CONCURRENT_DISPATCH",
  "COVENANT_DAEMON_RETRIES",
] as const;

const saved: Partial<Record<(typeof ENV_KEYS)[number], string | undefined>> = {};

const BASE: Record<string, string> = {
  // 32 chars: well past the 24-char floor.
  HATCHER_CONNECTOR_TOKEN: "t".repeat(32),
  COVENANT_OPERATOR_TOKEN: "operator-token-fallback-value",
};

async function loadConfig(): Promise<typeof import("./config.js")> {
  vi.resetModules();
  return import("./config.js");
}

describe("loadConfig env validation", () => {
  beforeEach(() => {
    for (const k of ENV_KEYS) saved[k] = process.env[k];
    for (const k of Object.keys(BASE)) delete process.env[k];
    Object.assign(process.env, BASE);
  });

  afterEach(() => {
    for (const k of ENV_KEYS) {
      if (saved[k] === undefined) delete process.env[k];
      else process.env[k] = saved[k];
    }
  });

  it("loads a valid base config", async () => {
    const { loadConfig: load } = await loadConfig();
    const cfg = load();
    expect(cfg.port).toBe(8790);
    expect(cfg.daemonUrl).toBe("http://127.0.0.1:8421");
    expect(cfg.usingConnectorPeer).toBe(false);
    expect(cfg.daemonToken).toBe(BASE.COVENANT_OPERATOR_TOKEN);
  });

  describe("HATCHER_CONNECTOR_TOKEN", () => {
    it("refuses a missing token", async () => {
      delete process.env.HATCHER_CONNECTOR_TOKEN;
      const { loadConfig: load } = await loadConfig();
      expect(() => load()).toThrow(/HATCHER_CONNECTOR_TOKEN/);
    });

    it("refuses a token shorter than 24 chars", async () => {
      process.env.HATCHER_CONNECTOR_TOKEN = "short";
      const { loadConfig: load } = await loadConfig();
      expect(() => load()).toThrow(/at least 24 chars/);
    });

    it("accepts a token of exactly the 24-char boundary", async () => {
      process.env.HATCHER_CONNECTOR_TOKEN = "t".repeat(24);
      const { loadConfig: load } = await loadConfig();
      expect(load().hatcherToken).toHaveLength(24);
    });
  });

  describe("int() parsing", () => {
    it("refuses a non-integer max-concurrent-dispatch", async () => {
      process.env.HATCHER_MAX_CONCURRENT_DISPATCH = "1.5";
      const { loadConfig: load } = await loadConfig();
      expect(() => load()).toThrow(/HATCHER_MAX_CONCURRENT_DISPATCH/);
    });

    it("refuses a below-min max-concurrent-dispatch (0)", async () => {
      process.env.HATCHER_MAX_CONCURRENT_DISPATCH = "0";
      const { loadConfig: load } = await loadConfig();
      expect(() => load()).toThrow(/HATCHER_MAX_CONCURRENT_DISPATCH/);
    });

    it("allows a zero daemon-retries because that field opts in at min 0", async () => {
      process.env.COVENANT_DAEMON_RETRIES = "0";
      const { loadConfig: load } = await loadConfig();
      expect(load().daemonRetries).toBe(0);
    });

    it("treats an empty int env as unset and applies the default", async () => {
      process.env.HATCHER_MAX_CONCURRENT_DISPATCH = "";
      const { loadConfig: load } = await loadConfig();
      expect(load().maxConcurrentDispatch).toBe(4);
    });
  });

  describe("PORT / HATCHER_CONNECTOR_PORT precedence", () => {
    it("uses HATCHER_CONNECTOR_PORT when PORT is unset", async () => {
      process.env.HATCHER_CONNECTOR_PORT = "9000";
      const { loadConfig: load } = await loadConfig();
      expect(load().port).toBe(9000);
    });

    it("PORT wins over HATCHER_CONNECTOR_PORT when both are set", async () => {
      process.env.PORT = "9000";
      process.env.HATCHER_CONNECTOR_PORT = "7777";
      const { loadConfig: load } = await loadConfig();
      expect(load().port).toBe(9000);
    });
  });

  describe("trailing-slash normalization", () => {
    it("strips trailing slashes from the daemon URL", async () => {
      process.env.COVENANT_DAEMON_URL = "http://127.0.0.1:8421///";
      const { loadConfig: load } = await loadConfig();
      expect(load().daemonUrl).toBe("http://127.0.0.1:8421");
    });

    it("strips trailing slashes from the Hatcher mesh URL", async () => {
      process.env.HATCHER_MESH_URL = "wss://mesh.hatcher.local/connector//";
      const { loadConfig: load } = await loadConfig();
      expect(load().hatcherUrl).toBe("wss://mesh.hatcher.local/connector");
    });
  });

  describe("daemon token source", () => {
    it("prefers the scoped connector peer and sets usingConnectorPeer", async () => {
      process.env.COVENANT_CONNECTOR_TOKEN = "scoped-connector-peer-token";
      const { loadConfig: load } = await loadConfig();
      const cfg = load();
      expect(cfg.usingConnectorPeer).toBe(true);
      expect(cfg.daemonToken).toBe("scoped-connector-peer-token");
    });

    it("falls back to the operator token when no connector peer is set", async () => {
      delete process.env.COVENANT_OPERATOR_TOKEN;
      process.env.COVENANT_OPERATOR_TOKEN = "op-token";
      const { loadConfig: load } = await loadConfig();
      const cfg = load();
      expect(cfg.usingConnectorPeer).toBe(false);
      expect(cfg.daemonToken).toBe("op-token");
    });

    it("refuses to boot with neither connector nor operator token", async () => {
      delete process.env.COVENANT_OPERATOR_TOKEN;
      const { loadConfig: load } = await loadConfig();
      expect(() => load()).toThrow(/COVENANT_OPERATOR_TOKEN/);
    });
  });

  describe("optional() whitespace handling", () => {
    it("drops a whitespace-only pairing code (treats it as unset)", async () => {
      process.env.HATCHER_PAIRING_CODE = "   ";
      const { loadConfig: load } = await loadConfig();
      expect(load().pairingCode).toBeUndefined();
    });

    it("trims a pairing code that has surrounding whitespace", async () => {
      process.env.HATCHER_PAIRING_CODE = "  pair123  ";
      const { loadConfig: load } = await loadConfig();
      expect(load().pairingCode).toBe("pair123");
    });
  });
});
