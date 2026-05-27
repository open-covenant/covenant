import { Sandbox, ALL_TRAFFIC, type SandboxOpts } from "e2b";
import type {
  Sandbox as ISandbox,
  SandboxProvider,
  SandboxSpec,
  ExecResult,
} from "../types.js";

// E2B's commands.run defaults to a 60s command timeout — far too short for a
// cold `npm install` / `next build`, which get killed mid-flight and make the
// agent thrash on a phantom-broken workspace. Give each command room up to a
// few minutes; the sandbox wall-clock (spec.wallMs) is still the hard ceiling.
const DEFAULT_CMD_TIMEOUT_MS = 300_000;

/**
 * E2B sandbox: an ephemeral Firecracker microVM, isolated from the gateway host
 * and from other runs. This is the real boundary that lets untrusted code run
 * without endangering covenant's infra.
 *
 * Residual hardening (follow-on within WS2, before fully-public launch):
 * - Egress allowlist: the base SDK gives the microVM open internet. Restricting
 *   outbound to `spec.egressAllowlist` requires a custom E2B template with
 *   firewall rules; until then a run can reach arbitrary hosts.
 * - Resource caps (cpu/memory/disk): template-level, not per-create in the base
 *   SDK. `spec` carries them for when a hardened template is wired.
 * `timeoutMs` is honored now as a teardown backstop — the microVM self-kills at
 * the wall-clock budget even if destroy() is never reached.
 */
class E2bSandbox implements ISandbox {
  constructor(private readonly sbx: Sandbox) {}

  readFile(path: string): Promise<string> {
    return this.sbx.files.read(path);
  }

  async writeFile(path: string, content: string): Promise<void> {
    await this.sbx.files.write(path, content);
  }

  async exec(cmd: string, opts: { timeoutMs?: number } = {}): Promise<ExecResult> {
    try {
      const r = await this.sbx.commands.run(cmd, {
        timeoutMs: opts.timeoutMs ?? DEFAULT_CMD_TIMEOUT_MS,
      });
      return { stdout: r.stdout, stderr: r.stderr, exitCode: r.exitCode };
    } catch (e) {
      // e2b throws on non-zero exit; surface the result fields rather than throw.
      const err = e as { stdout?: string; stderr?: string; exitCode?: number };
      if (typeof err.exitCode === "number") {
        return { stdout: err.stdout ?? "", stderr: err.stderr ?? "", exitCode: err.exitCode };
      }
      throw e;
    }
  }

  async previewUrl(port: number): Promise<string> {
    return `https://${this.sbx.getHost(port)}`;
  }

  async destroy(): Promise<void> {
    await this.sbx.kill();
  }
}

export class E2bSandboxProvider implements SandboxProvider {
  readonly id = "e2b";

  constructor(private readonly apiKey: string) {}

  async create(spec: SandboxSpec): Promise<ISandbox> {
    // Opt into the prebuilt `covenant-coder` template (more memory + warm npm
    // cache → reliable, fast heavy installs) when E2B_TEMPLATE is set; fall
    // back to the default base sandbox otherwise.
    const template = process.env.E2B_TEMPLATE?.trim();
    const opts: SandboxOpts = { apiKey: this.apiKey, timeoutMs: spec.wallMs };
    // Egress allowlist for untrusted public code: when E2B_EGRESS_ALLOW is set
    // (comma-separated domains/CIDRs), deny all outbound except those hosts so a
    // run can't be used to attack or spam arbitrary hosts. Unset = open (dev).
    // Domain matching is HTTP:80 / TLS:443 only — package registries are HTTPS,
    // so it covers npm/pip/git without breaking installs (verify before trusting).
    const allow = process.env.E2B_EGRESS_ALLOW?.split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    if (allow && allow.length > 0) {
      opts.network = { denyOut: [ALL_TRAFFIC], allowOut: allow };
    }
    const sbx = template
      ? await Sandbox.create(template, opts)
      : await Sandbox.create(opts);
    return new E2bSandbox(sbx);
  }
}
