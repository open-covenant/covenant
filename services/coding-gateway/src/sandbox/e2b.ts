import { Sandbox, ALL_TRAFFIC, type SandboxOpts } from 'e2b';
import type { Sandbox as ISandbox, SandboxProvider, SandboxSpec, ExecResult } from '../types.js';
import { validateE2bEgressPolicy, validateRunEgressAllowlist } from '../egress-policy.js';

// E2B's commands.run defaults to a 60s command timeout — far too short for a
// cold `npm install` / `next build`, which get killed mid-flight and make the
// agent thrash on a phantom-broken workspace. Give each command room up to a
// few minutes; the sandbox wall-clock (spec.wallMs) is still the hard ceiling.
const DEFAULT_CMD_TIMEOUT_MS = 300_000;

export interface E2bIdentityExpectation {
  templateId: string;
  cpuCount: number;
  memoryMb: number;
}

/**
 * E2B sandbox: an ephemeral Firecracker microVM, isolated from the gateway host
 * and from other runs. This is the real boundary that lets untrusted code run
 * without endangering covenant's infra.
 *
 * Outbound traffic is always denied by default. Each run may request only a
 * subset of the operator policy captured when this provider is constructed.
 * CPU, memory, and disk limits remain template-level controls in the E2B SDK.
 * `timeoutMs` is honored now as a teardown backstop — the microVM self-kills at
 * the wall-clock budget even if destroy() is never reached.
 *
 * E2B 2.27 exposes lifecycle and resource metrics, but not an authoritative
 * billing-cost receipt. The gateway therefore never derives spend from those
 * metrics: it retains the full pinned worst-case sandbox reservation for every
 * attempted create.
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
      if (typeof err.exitCode === 'number') {
        return { stdout: err.stdout ?? '', stderr: err.stderr ?? '', exitCode: err.exitCode };
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
  readonly id = 'e2b';
  private readonly egressPolicy: ReadonlySet<string>;

  constructor(
    private readonly apiKey: string,
    private readonly expected?: E2bIdentityExpectation,
    egressPolicy: readonly string[] = [],
  ) {
    this.egressPolicy = new Set(validateE2bEgressPolicy(egressPolicy));
  }

  async check(): Promise<void> {
    const page = Sandbox.list({ apiKey: this.apiKey, limit: 1 });
    await page.nextItems({ requestTimeoutMs: 15_000 });
  }

  async create(spec: SandboxSpec): Promise<ISandbox> {
    // Production passes the immutable template ID explicitly. E2B_TEMPLATE is
    // retained only as a local-development convenience and is rejected by the
    // production configuration contract.
    const template = this.expected?.templateId ?? process.env.E2B_TEMPLATE?.trim();
    const allow = validateRunEgressAllowlist(spec.egressAllowlist);
    for (const host of allow) {
      if (!this.egressPolicy.has(host)) {
        throw new Error(`sandbox egress host ${host} is outside the operator policy`);
      }
    }
    const opts: SandboxOpts = {
      apiKey: this.apiKey,
      timeoutMs: spec.wallMs,
      network: {
        denyOut: [ALL_TRAFFIC],
        ...(allow.length > 0 ? { allowOut: [...allow] } : {}),
      },
    };
    const sbx = template ? await Sandbox.create(template, opts) : await Sandbox.create(opts);
    if (this.expected) await this.assertIdentity(sbx, this.expected);
    return new E2bSandbox(sbx);
  }

  private async assertIdentity(sbx: Sandbox, expected: E2bIdentityExpectation): Promise<void> {
    try {
      const info = await sbx.getInfo();
      const actual = {
        templateId: info.templateId,
        cpuCount: info.cpuCount,
        memoryMb: info.memoryMB,
      };
      if (
        actual.templateId !== expected.templateId ||
        actual.cpuCount !== expected.cpuCount ||
        actual.memoryMb !== expected.memoryMb
      ) {
        throw new Error(
          `sandbox identity mismatch: expected template ${expected.templateId} ` +
            `${expected.cpuCount} vCPU/${expected.memoryMb} MiB, received ` +
            `${actual.templateId} ${actual.cpuCount} vCPU/${actual.memoryMb} MiB`,
        );
      }
    } catch (cause) {
      await sbx.kill().catch(() => undefined);
      throw cause;
    }
  }
}
