// Maps a Hatcher agent manifest into (1) the Covenant capability grants that the
// daemon ACTUALLY ENFORCES, and (2) a consent/sandbox policy for the things the
// capability layer does not gate today. Both are surfaced to Hatcher so the UI can
// render an honest consent prompt.
//
// Verified against covenantd source in this worktree:
//  - Tool-call enforcement keys on `tool.call.<name>` (exact action match) + scope,
//    deny-by-default, no operator/trust-root bypass, always on
//    (covenantd/src/lib.rs:3403-3556). Grants bind to the CALLER's pubkey
//    (lib.rs:4330) — so the connector must dispatch under a scoped, non-operator peer
//    for these to actually constrain anything.
//  - The coding agent's tools (read_file/write_file/edit_file/bash) run INSIDE the
//    coding-gateway sandbox and are NOT mediated by covenant capabilities today; they
//    are governed by the sandbox (gVisor/E2B) and recorded in the audit trail. So
//    filesystem/terminal/browser manifest entries map to a CONSENT POLICY, not to
//    (decorative, deny-all) capability grants. Bringing them under token control needs
//    the gateway to delegate tool execution to the daemon's enforced /tools/call path
//    — a roadmap item (see docs/hatcher-handoff.md §5).
//  - Enforced-today grants: intent baseline (memory.write, Stage-1 gate lib.rs:3654),
//    daemon-side MCP/native tools (tool.call.<name>), and A2A (a2a.<verb>.<peer>).
//  - Scope predicate is exact-JSON match; structured path/argv/domain predicates are
//    not interpreted yet (deferred), so scoped grants bind the tool NAME only and
//    leave arguments unconstrained rather than emitting an allow-rule that exact-match
//    would treat as deny-all.

import type { GrantRequest } from './daemon.js';
import type { ManifestCapability } from './manifest.js';
import type { MeshGrant } from './transport.js';

export interface ConsentPolicy {
  fs?: { read: string[]; write: string[] };
  exec?: { argv0Allow?: string[]; approval?: string; timeout_ms?: number };
  net?: { domains: string[] };
  github?: { scopes?: string[]; repo?: string };
  mcpWildcard?: boolean;
  // Human-readable notes on why an entry is sandbox/consent-governed vs token-enforced.
  notes: string[];
}

export interface MappedManifest {
  grants: GrantRequest[];
  policy: ConsentPolicy;
}

// Caps the connector peer needs for ANY intent to run: the Stage-1 submission gate
// requires memory.write (covenantd/src/lib.rs:3654). Minted on every dispatch,
// expiring at the deadline. Agent-card-required caps are layered on per agent.
export function baselineGrants(expiresAt: number): GrantRequest[] {
  return [{ action: 'memory.write', expires_at: expiresAt }];
}

export function mapManifest(caps: ManifestCapability[], expiresAt: number): MappedManifest {
  const grants: GrantRequest[] = [];
  const policy: ConsentPolicy = { notes: [] };

  for (const cap of caps) {
    switch (cap.tool) {
      case 'filesystem': {
        const fs = (policy.fs ??= { read: [], write: [] });
        (cap.mode === 'read' ? fs.read : fs.write).push(...cap.paths);
        policy.notes.push(`filesystem.${cap.mode} is sandbox-enforced + audited (not token-gated in v0)`);
        break;
      }
      case 'terminal': {
        const exec = (policy.exec ??= { argv0Allow: [] });
        (exec.argv0Allow ??= []).push(...cap.commands.map(firstToken));
        policy.notes.push('terminal (bash) is sandbox-enforced + audited (not token-gated in v0)');
        break;
      }
      case 'browser': {
        const net = (policy.net ??= { domains: [] });
        net.domains.push(...cap.domains);
        policy.notes.push('browser/network egress is sandbox-enforced (not token-gated in v0)');
        break;
      }
      case 'github':
        // GitHub is reachable as a daemon-side MCP tool -> REAL tool.call.github gate.
        // Scope binds the tool name only; the requested gh scopes are advisory until
        // the tool's argument schema is pinned (exact-match would otherwise deny-all).
        grants.push({ action: 'tool.call.github', scope: { version: 1, tool: 'github' }, expires_at: expiresAt });
        policy.github = { scopes: [...(policy.github?.scopes ?? []), ...cap.scopes] };
        break;
      case 'mcp':
        for (const name of cap.tools) {
          if (name === '*') {
            // No single capability can match every `tool.call.<name>` under exact
            // action matching — wildcard is not enforceable; enumerate tool names.
            policy.mcpWildcard = true;
            policy.notes.push('mcp "*" is not enforceable as a single grant; enumerate tool names');
            continue;
          }
          grants.push({ action: `tool.call.${name}`, scope: { version: 1, tool: name }, expires_at: expiresAt });
        }
        break;
      case 'a2a':
        for (const act of cap.actions) {
          for (const peer of cap.peers) {
            grants.push({ action: `a2a.${act}.${peer}`, scope: { peer_pubkey_b58: peer }, expires_at: expiresAt });
          }
        }
        break;
    }
  }

  return { grants, policy };
}

// Maps Hatcher's per-dispatch grants ({scope, constraints}, sent in the dispatch frame)
// into covenant grants + consent policy. Same honest tiering as mapManifest:
// filesystem/terminal/browser are sandbox-enforced (consent policy), github/mcp/a2a are
// token-gated covenant grants.
export function mapMeshGrants(grants: MeshGrant[], expiresAt: number): MappedManifest {
  const out: GrantRequest[] = [];
  const policy: ConsentPolicy = { notes: [] };

  for (const g of grants) {
    const c = (g.constraints ?? {}) as Record<string, unknown>;
    switch (g.scope.split('.')[0]) {
      case 'filesystem': {
        const fs = (policy.fs ??= { read: [], write: [] });
        const paths = Array.isArray(c.paths) ? (c.paths as string[]) : [];
        (g.scope.endsWith('.write') ? fs.write : fs.read).push(...paths);
        policy.notes.push(`${g.scope} is sandbox-enforced + audited (not token-gated)`);
        break;
      }
      case 'terminal': {
        const exec = (policy.exec ??= {});
        if (typeof c.approval === 'string') exec.approval = c.approval;
        if (typeof c.timeout_ms === 'number') exec.timeout_ms = c.timeout_ms;
        policy.notes.push(`${g.scope} is sandbox-enforced + audited (not token-gated)`);
        break;
      }
      case 'browser':
      case 'network': {
        const net = (policy.net ??= { domains: [] });
        if (Array.isArray(c.domains)) net.domains.push(...(c.domains as string[]));
        policy.notes.push(`${g.scope} egress is sandbox-enforced (not token-gated)`);
        break;
      }
      case 'github': {
        out.push({ action: 'tool.call.github', scope: { version: 1, tool: 'github' }, expires_at: expiresAt });
        policy.github = {
          scopes: [...(policy.github?.scopes ?? []), g.scope],
          ...(typeof c.repo === 'string' ? { repo: c.repo } : {}),
        };
        break;
      }
      case 'mcp': {
        const tool = g.scope.split('.')[1];
        if (tool && tool !== '*') {
          out.push({ action: `tool.call.${tool}`, scope: { version: 1, tool }, expires_at: expiresAt });
        } else {
          policy.mcpWildcard = true;
          policy.notes.push('mcp wildcard is not enforceable as a single grant; enumerate tool names');
        }
        break;
      }
      case 'a2a': {
        const verb = g.scope.split('.')[1];
        const peer = typeof c.peer === 'string' ? c.peer : undefined;
        if (verb && peer) out.push({ action: `a2a.${verb}.${peer}`, scope: { peer_pubkey_b58: peer }, expires_at: expiresAt });
        break;
      }
      default:
        policy.notes.push(`unrecognized grant scope "${g.scope}" — ignored`);
    }
  }

  return { grants: out, policy };
}

function firstToken(command: string): string {
  return command.trim().split(/\s+/)[0] ?? command;
}
