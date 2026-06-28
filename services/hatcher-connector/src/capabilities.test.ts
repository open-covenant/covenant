import { describe, it, expect } from 'vitest';
import { mapManifest, mapMeshGrants, baselineGrants } from './capabilities.js';

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

describe('mapMeshGrants — Hatcher per-dispatch grants ({scope, constraints})', () => {
  it('token-gates github, sandbox-gates filesystem/terminal into the consent policy', () => {
    const { grants, policy } = mapMeshGrants(
      [
        { scope: 'filesystem.read', constraints: { paths: ['./'] } },
        { scope: 'filesystem.write', constraints: { paths: ['./out'] } },
        { scope: 'terminal.exec', constraints: { approval: 'required', timeout_ms: 120000 } },
        { scope: 'github.read', constraints: { repo: 'owner/repo' } },
      ],
      EXP,
    );

    expect(grants).toEqual([{ action: 'tool.call.github', scope: { version: 1, tool: 'github' }, expires_at: EXP }]);
    expect(policy.fs).toEqual({ read: ['./'], write: ['./out'] });
    expect(policy.exec).toEqual({ approval: 'required', timeout_ms: 120000 });
    expect(policy.github).toEqual({ scopes: ['github.read'], repo: 'owner/repo' });
  });

  it('maps named mcp + a2a scopes to enforced grants and flags unknown scopes', () => {
    const { grants, policy } = mapMeshGrants(
      [
        { scope: 'mcp.summarize' },
        { scope: 'a2a.send', constraints: { peer: 'PKa' } },
        { scope: 'quantum.teleport' },
      ],
      EXP,
    );
    expect(grants.map((g) => g.action)).toEqual(['tool.call.summarize', 'a2a.send.PKa']);
    expect(policy.notes.some((n) => n.includes('quantum.teleport'))).toBe(true);
  });

  it('fails safe: no grant for a peerless a2a or an mcp wildcard; browser routes to consent policy', () => {
    const { grants, policy } = mapMeshGrants(
      [
        { scope: 'a2a.send' },
        { scope: 'mcp.*' },
        { scope: 'browser.open', constraints: { domains: ['docs.rs'] } },
      ],
      EXP,
    );
    expect(grants).toEqual([]);
    expect(policy.mcpWildcard).toBe(true);
    expect(policy.net).toEqual({ domains: ['docs.rs'] });
  });

  it('ignores malformed terminal constraint types so a bad dispatch frame cannot corrupt the exec policy', () => {
    // constraints arrive untyped (Record<string, unknown>). A non-string
    // approval or non-number timeout_ms must be dropped by the typeof guards,
    // not written into the sandbox consent policy as a truthy-but-meaningless
    // value that would weaken the approval gate or the wall-clock ceiling.
    const { grants, policy } = mapMeshGrants(
      [{ scope: 'terminal.exec', constraints: { approval: 123, timeout_ms: 'soon' } }],
      EXP,
    );
    expect(grants).toEqual([]);
    expect(policy.exec).toEqual({});
  });

  it('fails safe on a verbless a2a scope even when a peer is supplied', () => {
    // 'a2a' with no verb segment (split('.')[1] is undefined) must not emit an
    // `a2a.undefined.<peer>` grant — the verb half of the `verb && peer` guard
    // fails closed just like the peerless half above.
    const { grants } = mapMeshGrants([{ scope: 'a2a', constraints: { peer: 'PKa' } }], EXP);
    expect(grants).toEqual([]);
  });

  it('drops a non-array filesystem paths constraint instead of spreading it into the sandbox allowlist', () => {
    // A dispatch frame sending paths as a bare string (not the expected array)
    // must fail closed to an empty fs scope; without the Array.isArray guard the
    // string would spread character-by-character into the sandbox allowlist.
    const { grants, policy } = mapMeshGrants([{ scope: 'filesystem.read', constraints: { paths: '/etc' } }], EXP);
    expect(grants).toEqual([]);
    expect(policy.fs).toEqual({ read: [], write: [] });
  });

  it('drops a non-array browser/network domains constraint instead of spreading it into the egress allowlist', () => {
    const { policy } = mapMeshGrants([{ scope: 'browser.open', constraints: { domains: 'evil.com' } }], EXP);
    expect(policy.net).toEqual({ domains: [] });
  });

  it('omits a non-string github repo constraint rather than binding it to the github capability scope', () => {
    const { grants, policy } = mapMeshGrants([{ scope: 'github.read', constraints: { repo: 123 } }], EXP);
    expect(grants).toEqual([{ action: 'tool.call.github', scope: { version: 1, tool: 'github' }, expires_at: EXP }]);
    expect(policy.github).toEqual({ scopes: ['github.read'] });
  });
});
