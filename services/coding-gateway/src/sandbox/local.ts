import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname, sep } from "node:path";
import { exec } from "node:child_process";
import type { Sandbox, SandboxProvider, SandboxSpec, ExecResult } from "../types.js";

/**
 * Trusted-local sandbox: a scratch directory plus child-process exec on the
 * gateway host. It does NOT enforce the isolation contract — no egress
 * allowlist, no cpu/memory/disk caps, no secret stripping. Use it only for
 * local development; the E2B provider (coder-07) is the real boundary, and the
 * gateway must not be publicly exposed while running on this provider.
 */
class LocalSandbox implements Sandbox {
  constructor(private readonly root: string) {}

  private safe(p: string): string {
    const abs = resolve(this.root, p);
    if (abs !== this.root && !abs.startsWith(this.root + sep)) {
      throw new Error(`path escapes workspace: ${p}`);
    }
    return abs;
  }

  async readFile(p: string): Promise<string> {
    return readFile(this.safe(p), "utf8");
  }

  async writeFile(p: string, content: string): Promise<void> {
    const abs = this.safe(p);
    await mkdir(dirname(abs), { recursive: true });
    await writeFile(abs, content, "utf8");
  }

  exec(cmd: string, opts: { timeoutMs?: number } = {}): Promise<ExecResult> {
    return new Promise((resolveExec) => {
      exec(
        cmd,
        {
          cwd: this.root,
          timeout: opts.timeoutMs ?? 120_000,
          maxBuffer: 16 * 1024 * 1024,
          shell: "/bin/bash",
        },
        (err, stdout, stderr) => {
          const code =
            err && typeof (err as { code?: number }).code === "number"
              ? (err as { code: number }).code
              : err
                ? 1
                : 0;
          resolveExec({ stdout: stdout ?? "", stderr: stderr ?? "", exitCode: code });
        },
      );
    });
  }

  async previewUrl(port: number): Promise<string> {
    // Loopback only. The E2B provider returns a real public preview URL; the
    // local provider can't expose the sandbox to the browser.
    return `http://127.0.0.1:${port}`;
  }

  async destroy(): Promise<void> {
    await rm(this.root, { recursive: true, force: true });
  }
}

export class LocalSandboxProvider implements SandboxProvider {
  readonly id = "local";

  async create(spec: SandboxSpec): Promise<Sandbox> {
    const base = join(tmpdir(), "covenant-coder");
    await mkdir(base, { recursive: true });
    const root = await mkdtemp(join(base, `${spec.runId}-`));
    return new LocalSandbox(root);
  }
}
