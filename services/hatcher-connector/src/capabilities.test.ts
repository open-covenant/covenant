import { describe, it, expect } from 'vitest';
import { mapManifest, baselineGrants } from './capabilities.js';

const EXP = 1_900_000_000_000;

describe('baselineGrants', () => {
  it('mints memory.write for the Stage-1 intent gate, expiring at the deadline', () => {
    expect(baselineGrants(EXP)).toEqual([{ action: 'memory.write', expires_at: EXP }]);
  });
});

describe('mapManifest — enforced grants', () => {
  it('maps github to a daemon-side tool.call.github grant (tool-name bound)', () => {
    const { grants } = mapManifest([{ tool: 'github', scopes: ['repo:read'] }], EXP);
    expect(grants).toEqual([{ action: 'tool.call.github', scope: { version: 1, tool: 'github' }, expires_at: EXP }]);
  });

  it('maps named mcp tools to tool.call.<name> and flags an unenforceable "*"', () => {
    const named = mapManifest([{ tool: 'mcp', servers: ['x'], tools: ['summarize'] }], EXP);
    expect(named.grants).toEqual([{ action: 'tool.call.summarize', scope: { version: 1, tool: 'summarize' }, expires_at: EXP }]);

    const star = mapManifest([{ tool: 'mcp', servers: ['x'], tools: ['*'] }], EXP);
    expect(star.grants).toEqual([]);
    expect(star.policy.mcpWildcard).toBe(true);
  });

  it('expands a2a into per-(action,peer) peer-scoped grants', () => {
    const { grants } = mapManifest([{ tool: 'a2a', peers: ['PKa', 'PKb'], actions: ['send', 'respond'] }], EXP);
    expect(grants.map((g) => g.action)).toEqual(['a2a.send.PKa', 'a2a.send.PKb', 'a2a.respond.PKa', 'a2a.respond.PKb']);
    expect(grants[0]!.scope).toEqual({ peer_pubkey_b58: 'PKa' });
  });
});

describe('mapManifest — consent policy (sandbox/audit governed, not token grants)', () => {
  it('routes filesystem to policy.fs, not a capability grant (in-sandbox fs is not token-gated)', () => {
    const { grants, policy } = mapManifest(
      [
        { tool: 'filesystem', mode: 'read', paths: ['./'] },
        { tool: 'filesystem', mode: 'write', paths: ['./src'] },
      ],
      EXP,
    );
    expect(grants).toEqual([]);
    expect(policy.fs).toEqual({ read: ['./'], write: ['./src'] });
  });

  it('routes terminal to policy.exec.argv0Allow using the first token of each command', () => {
    const { grants, policy } = mapManifest([{ tool: 'terminal', commands: ['pnpm test', 'cargo test'] }], EXP);
    expect(grants).toEqual([]);
    expect(policy.exec).toEqual({ argv0Allow: ['pnpm', 'cargo'] });
  });

  it('routes browser to policy.net.domains', () => {
    const { policy } = mapManifest([{ tool: 'browser', domains: ['docs.rs'] }], EXP);
    expect(policy.net).toEqual({ domains: ['docs.rs'] });
  });
});
