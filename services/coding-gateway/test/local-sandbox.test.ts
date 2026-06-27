import { describe, it, expect, afterEach } from "vitest";
import { basename } from "node:path";
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

  it("rejects a sibling directory sharing the workspace name as a prefix", async () => {
    sandbox = await make("sibling-prefix");
    // The `../escape.txt` cases resolve to paths that don't share the root as a
    // prefix at all, so they never test the `+ sep` boundary in safe(). A sibling
    // named "<root-basename>-leak" DOES start with this.root as a string prefix —
    // only the trailing separator distinguishes it. Discover the real basename via
    // the sandbox's own cwd (the random mkdtemp suffix is otherwise unknowable).
    const root = (await sandbox.exec("pwd")).stdout.trim();
    const sibling = `../${basename(root)}-leak/f.txt`;
    await expect(sandbox.readFile(sibling)).rejects.toThrow(/escapes workspace/);
    await expect(sandbox.writeFile(sibling, "x")).rejects.toThrow(/escapes workspace/);
    await sandbox.writeFile("nested/ok.txt", "in");
    expect(await sandbox.readFile("nested/ok.txt")).toBe("in");
  });

  it("destroy removes the workspace", async () => {
    const s = await make("destroy");
    await s.writeFile("f.txt", "x");
    await s.destroy();
    await expect(s.readFile("f.txt")).rejects.toThrow();
  });
});
