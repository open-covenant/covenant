// Shared demo fixtures + a scripted DaemonClient for the offline demo: emits a
// realistic coding trace (bash test run + file write) and a verified audit root,
// with no network or LLM. The LIVE demos swap ScriptedDaemon for HttpDaemonClient
// against a real covenantd.

import type { DaemonClient, AgentEvent, IntentResult, IntegrityReport, GrantRequest } from '../daemon.js';
import type { ManifestCapability } from '../manifest.js';

export const DEMO_CAPS: ManifestCapability[] = [
  { tool: 'filesystem', mode: 'read', paths: ['./'] },
  { tool: 'filesystem', mode: 'write', paths: ['./'] },
  { tool: 'terminal', commands: ['pnpm test', 'git status'] },
  { tool: 'github', scopes: ['repo:read', 'pr:comment'] },
];

export const DEMO_TRACE: AgentEvent[] = [
  { type: 'tool_call', run_id: 'r1', tool: 'bash', preview: 'pnpm test' },
  { type: 'tool_result', run_id: 'r1', tool: 'bash', duration_ms: 8200, error: false },
  { type: 'tool_call', run_id: 'r1', tool: 'read_file', preview: 'src/index.ts' },
  { type: 'file_write', run_id: 'r1', path: 'REPORT.md', bytes: 2048 },
];

export class ScriptedDaemon implements DaemonClient {
  readonly grants: GrantRequest[] = [];

  constructor(
    private readonly events: AgentEvent[],
    private readonly opts: { intentId?: string; root?: string; resultText?: string } = {},
  ) {}

  private get id(): string {
    return this.opts.intentId ?? 'demo-intent';
  }

  async health(): Promise<boolean> {
    return true;
  }

  async submitIntent(text: string): Promise<IntentResult> {
    return { intent_id: this.id, status: 'running', text, sources: ['hermes'] };
  }

  async intentResult(): Promise<unknown> {
    return {
      kind: 'intent_result',
      intent_id: this.id,
      status: 'ok',
      text: this.opts.resultText ?? 'Test suite: 42 passed, 0 failed. Wrote REPORT.md.',
      sources: ['hermes'],
    };
  }

  async streamEvents(_id: string, onEvent: (e: AgentEvent) => void): Promise<void> {
    for (const e of this.events) onEvent(e);
  }

  async verify(): Promise<IntegrityReport> {
    return { events: this.events.length + 2, anchors: 1, valid: true, root_hash_hex: this.opts.root ?? 'a1b2c3d4e5f60718', failures: [] };
  }

  async grant(req: GrantRequest): Promise<void> {
    this.grants.push(req);
  }
}
