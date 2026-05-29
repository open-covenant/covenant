import { describe, it, expect, afterEach } from "vitest";
import { LocalSandboxProvider } from "../src/sandbox/local.js";
import type { Sandbox } from "../src/types.js";

const provider = new LocalSandboxProvider();
let sandbox: Sandbox | undefined;

function make(runId: string): Promise<Sandbox> {
  return provider.create({
    runId,
    egressAllowlist: [],
    cpuMs: 60_000,
    memoryMb: 512,
    diskMb: 512,
    wallMs: 60_000,
  });
}

afterEach(async () => {
  await sandbox?.destroy();
  sandbox = undefined;
});

describe("LocalSandbox", () => {
  it("round-trips a file through nested dirs", async () => {
    sandbox = await make("write-read");
    await sandbox.writeFile("src/app.ts", "export const x = 1;\n");
    expect(await sandbox.readFile("src/app.ts")).toBe("export const x = 1;\n");
  });

  it("runs a command in the workspace", async () => {
    sandbox = await make("exec");
    await sandbox.writeFile("hello.txt", "hi");
    const r = await sandbox.exec("cat hello.txt");
    expect(r.exitCode).toBe(0);
    expect(r.stdout).toBe("hi");
  });

  it("reports a non-zero exit code without throwing", async () => {
    sandbox = await make("exit-code");
    const r = await sandbox.exec("exit 3");
    expect(r.exitCode).toBe(3);
  });

  it("rejects path traversal outside the workspace", async () => {
    sandbox = await make("traversal");
    await expect(sandbox.writeFile("../escape.txt", "x")).rejects.toThrow(/escapes workspace/);
    await expect(sandbox.readFile("../../etc/hosts")).rejects.toThrow(/escapes workspace/);
  });

  it("destroy removes the workspace", async () => {
    const s = await make("destroy");
    await s.writeFile("f.txt", "x");
    await s.destroy();
    await expect(s.readFile("f.txt")).rejects.toThrow();
  });
});
