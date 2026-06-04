import { describe, it, expect } from 'vitest';
import { parseManifest } from './manifest.js';

const BASE = {
  spec: 'covenant.hatcher-agent.v0',
  agent: { name: 'repo-doctor', pubkey: 'A'.repeat(43) },
  runner: { kind: 'covenant-local' },
};

describe('parseManifest', () => {
  it('applies defaults for runner protocol, limits, and proof', () => {
    const m = parseManifest({ ...BASE, capabilities: [] });
    expect(m.runner.min_daemon_protocol).toBe(2);
    expect(m.limits.max_files).toBe(40);
    expect(m.proof.spec).toBe('covenant.connector-trace.v0');
  });

  it('accepts the full capability union', () => {
    const m = parseManifest({
      ...BASE,
      capabilities: [
        { tool: 'filesystem', mode: 'write', paths: ['./'] },
        { tool: 'terminal', commands: ['pnpm test'] },
        { tool: 'github', scopes: ['repo:read'] },
        { tool: 'browser', domains: ['docs.rs'] },
        { tool: 'mcp', servers: ['@covenant/mcp-bridge'], tools: ['*'] },
        { tool: 'a2a', peers: ['PK'], actions: ['send'] },
      ],
    });
    expect(m.capabilities).toHaveLength(6);
  });

  it('rejects an unknown spec', () => {
    expect(() => parseManifest({ ...BASE, spec: 'nope' })).toThrow();
  });

  it('rejects an unknown tool in capabilities', () => {
    expect(() => parseManifest({ ...BASE, capabilities: [{ tool: 'kernel', mode: 'read' }] })).toThrow();
  });

  it('rejects a too-short pubkey', () => {
    expect(() => parseManifest({ ...BASE, agent: { name: 'x', pubkey: 'short' } })).toThrow();
  });
});
