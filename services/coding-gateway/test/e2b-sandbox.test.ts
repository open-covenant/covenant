import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SandboxSpec } from "../src/types.js";

const { createMock, ALL_TRAFFIC } = vi.hoisted(() => ({
  createMock: vi.fn(),
  ALL_TRAFFIC: "all-traffic-sentinel",
}));

vi.mock("e2b", () => ({
  Sandbox: { create: createMock },
  ALL_TRAFFIC,
}));

import { E2bSandboxProvider } from "../src/sandbox/e2b.js";

const SPEC: SandboxSpec = {
  runId: "run-1",
  egressAllowlist: [],
  cpuMs: 60_000,
  memoryMb: 512,
  diskMb: 512,
  wallMs: 90_000,
};

// create(opts) | create(template, opts) — opts is always the last argument.
function lastOpts(): any {
  return createMock.mock.calls.at(-1)!.at(-1);
}

describe("E2bSandboxProvider egress firewall", () => {
  beforeEach(() => {
    createMock.mockReset();
    createMock.mockResolvedValue({});
    delete process.env.E2B_EGRESS_ALLOW;
    delete process.env.E2B_TEMPLATE;
  });
  afterEach(() => {
    delete process.env.E2B_EGRESS_ALLOW;
    delete process.env.E2B_TEMPLATE;
  });

  it("leaves egress open and threads apiKey + wall-clock backstop when E2B_EGRESS_ALLOW is unset", async () => {
    await new E2bSandboxProvider("key-abc").create(SPEC);
    const opts = lastOpts();
    expect(opts.network).toBeUndefined();
    expect(opts.apiKey).toBe("key-abc");
    expect(opts.timeoutMs).toBe(SPEC.wallMs);
  });

  it("denies all outbound and allows only the configured hosts when E2B_EGRESS_ALLOW is set", async () => {
    process.env.E2B_EGRESS_ALLOW = "registry.npmjs.org, api.anthropic.com";
    await new E2bSandboxProvider("key-abc").create(SPEC);
    expect(lastOpts().network).toEqual({
      denyOut: [ALL_TRAFFIC],
      allowOut: ["registry.npmjs.org", "api.anthropic.com"],
    });
  });

  it("filters a trailing-comma blank so the firewall stays scoped to the real host", async () => {
    process.env.E2B_EGRESS_ALLOW = "registry.npmjs.org, ";
    await new E2bSandboxProvider("key-abc").create(SPEC);
    expect(lastOpts().network.allowOut).toEqual(["registry.npmjs.org"]);
  });

  it("opts into a trimmed E2B_TEMPLATE and falls back to the base sandbox otherwise", async () => {
    process.env.E2B_TEMPLATE = "  covenant-coder  ";
    await new E2bSandboxProvider("k").create(SPEC);
    expect(createMock.mock.calls.at(-1)).toEqual([
      "covenant-coder",
      expect.objectContaining({ apiKey: "k" }),
    ]);

    createMock.mockClear();
    process.env.E2B_TEMPLATE = "   ";
    await new E2bSandboxProvider("k").create(SPEC);
    expect(createMock.mock.calls.at(-1)).toHaveLength(1);
  });
});

describe("E2bSandbox.exec result surfacing", () => {
  beforeEach(() => {
    createMock.mockReset();
    delete process.env.E2B_EGRESS_ALLOW;
    delete process.env.E2B_TEMPLATE;
  });

  // create() returns `new E2bSandbox(sbx)`, so a fake `sbx.commands.run`
  // drives the exec error-surfacing arm without a live microVM.
  async function sandboxWithRun(run: (cmd: string, o: { timeoutMs: number }) => Promise<unknown>) {
    createMock.mockResolvedValue({ commands: { run } });
    return new E2bSandboxProvider("k").create(SPEC);
  }

  it("returns the command result and defaults the per-command timeout backstop", async () => {
    const seen: { timeoutMs: number }[] = [];
    const sbx = await sandboxWithRun(async (_cmd, o) => {
      seen.push(o);
      return { stdout: "ok", stderr: "", exitCode: 0 };
    });
    expect(await sbx.exec("npm test")).toEqual({ stdout: "ok", stderr: "", exitCode: 0 });
    await sbx.exec("npm test", { timeoutMs: 5_000 });
    expect(seen.map((o) => o.timeoutMs)).toEqual([300_000, 5_000]);
  });

  it("surfaces an e2b non-zero exit as a result rather than throwing", async () => {
    const sbx = await sandboxWithRun(async () => {
      throw { stdout: "build out", stderr: "boom", exitCode: 2 };
    });
    expect(await sbx.exec("npm run build")).toEqual({ stdout: "build out", stderr: "boom", exitCode: 2 });
  });

  it("defaults absent stdout/stderr to empty strings on a surfaced failure", async () => {
    const sbx = await sandboxWithRun(async () => {
      throw { exitCode: 1 };
    });
    expect(await sbx.exec("false")).toEqual({ stdout: "", stderr: "", exitCode: 1 });
  });

  it("rethrows a transport error that carries no exit code", async () => {
    const sbx = await sandboxWithRun(async () => {
      throw new Error("microVM unreachable");
    });
    await expect(sbx.exec("npm test")).rejects.toThrow(/microVM unreachable/);
  });
});
