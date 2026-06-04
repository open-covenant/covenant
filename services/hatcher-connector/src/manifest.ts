// Manifest schema for Hatcher-hosted agents that target a Covenant local runner.
// Spec: covenant.hatcher-agent.v0 (see docs/hatcher-connector-design.md §4).
// The manifest is an ALLOWLIST: absence of a capability means denied.

import { z } from 'zod';

const FilesystemCap = z.object({
  tool: z.literal('filesystem'),
  mode: z.enum(['read', 'write']),
  paths: z.array(z.string()).min(1),
});

const TerminalCap = z.object({
  tool: z.literal('terminal'),
  commands: z.array(z.string()).min(1),
});

const GithubCap = z.object({
  tool: z.literal('github'),
  scopes: z.array(z.string()).min(1),
});

const BrowserCap = z.object({
  tool: z.literal('browser'),
  domains: z.array(z.string()).min(1),
});

const McpCap = z.object({
  tool: z.literal('mcp'),
  servers: z.array(z.string()).min(1),
  tools: z.array(z.string()).min(1),
});

const A2aCap = z.object({
  tool: z.literal('a2a'),
  peers: z.array(z.string()).min(1),
  actions: z.array(z.enum(['send', 'recv', 'respond'])).min(1),
});

export const ManifestCapability = z.discriminatedUnion('tool', [
  FilesystemCap,
  TerminalCap,
  GithubCap,
  BrowserCap,
  McpCap,
  A2aCap,
]);
export type ManifestCapability = z.infer<typeof ManifestCapability>;

export const HatcherAgentManifest = z.object({
  spec: z.literal('covenant.hatcher-agent.v0'),
  agent: z.object({
    name: z.string().min(1),
    pubkey: z.string().min(32), // base58 daemon identity this manifest is paired to
    description: z.string().default(''),
  }),
  runner: z.object({
    kind: z.literal('covenant-local'),
    min_daemon_protocol: z.number().int().min(1).default(2),
    requires: z.array(z.string()).default([]),
  }),
  capabilities: z.array(ManifestCapability).default([]),
  limits: z
    .object({
      max_run_ms: z.number().int().min(1).default(1_800_000),
      max_files: z.number().int().min(1).default(40),
      deadline_required: z.boolean().default(true),
    })
    .prefault({}),
  proof: z
    .object({
      spec: z.literal('covenant.connector-trace.v0').default('covenant.connector-trace.v0'),
      audit_root: z.boolean().default(true),
    })
    .prefault({}),
});
export type HatcherAgentManifest = z.infer<typeof HatcherAgentManifest>;

export function parseManifest(input: unknown): HatcherAgentManifest {
  return HatcherAgentManifest.parse(input);
}
