import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import type { Tool } from '@modelcontextprotocol/sdk/types.js';
import { z } from 'zod';

type TextContent = {
  type: 'text';
  text: string;
};

export type ToolResult = {
  content: TextContent[];
  isError?: boolean;
};

type ToolInputSchema = Tool['inputSchema'];
type Fetch = typeof fetch;

const DEFAULT_DAEMON_URL = 'http://127.0.0.1:8421';

const limitSchema = z.object({
  limit: z.number().int().positive().max(500).optional(),
});

const memoryTierSchema = z.enum(['working', 'episodic', 'longterm']);

const memoryRecentSchema = limitSchema.extend({
  tier: memoryTierSchema.optional(),
});

const memorySearchSchema = memoryRecentSchema.extend({
  q: z.string().min(1),
});

const toolCallSchema = z.object({
  name: z.string().min(1),
  arguments: z.unknown().optional(),
});

const grantCapabilitySchema = z.object({
  action: z.string().min(1),
  scope: z.unknown().optional(),
  expires_at: z.number().int().nonnegative().optional(),
});

const revokeCapabilitySchema = z.object({
  signature_b58: z.string().min(1),
});

const looseObjectSchema = z.record(z.string(), z.unknown());

function describeTool(name: string, description: string, inputSchema: ToolInputSchema): Tool {
  return {
    name,
    description,
    inputSchema,
  };
}

function asText(value: unknown): ToolResult {
  return {
    content: [
      {
        type: 'text',
        text: JSON.stringify(value, null, 2),
      },
    ],
  };
}

function asError(message: string): ToolResult {
  return {
    content: [
      {
        type: 'text',
        text: message,
      },
    ],
    isError: true,
  };
}

function daemonBaseUrl(env: NodeJS.ProcessEnv): string {
  return (env.COVENANT_HTTP_URL ?? DEFAULT_DAEMON_URL).replace(/\/+$/, '');
}

function tokenPath(env: NodeJS.ProcessEnv): string {
  const home = env.COVENANT_HOME ?? path.join(env.HOME ?? homedir(), '.covenant');
  return path.join(home, 'peers', 'operator.token');
}

function tokenPathLabel(env: NodeJS.ProcessEnv): string {
  if (env.COVENANT_HOME) return '$COVENANT_HOME/peers/operator.token';
  return '$HOME/.covenant/peers/operator.token';
}

function authToken(env: NodeJS.ProcessEnv): string {
  const explicit = env.COVENANT_AUTH_TOKEN?.trim();
  if (explicit) return explicit;

  const file = tokenPath(env);
  if (existsSync(file)) {
    const token = readFileSync(file, 'utf8').trim();
    if (token) return token;
  }

  throw new Error(
    `COVENANT_AUTH_TOKEN is not set and no token was readable at ${tokenPathLabel(env)}`,
  );
}

function redactSensitive(input: string, env: NodeJS.ProcessEnv): string {
  let out = input;
  for (const value of [env.COVENANT_AUTH_TOKEN, env.COVENANT_HOME, env.HOME]) {
    if (value) {
      out = out
        .split(value)
        .join(value === env.COVENANT_AUTH_TOKEN ? '<redacted-token>' : '$HOME');
    }
  }

  const file = tokenPath(env);
  out = out.split(file).join(tokenPathLabel(env));
  return out;
}

function appendParams(url: URL, params: Record<string, string | number | undefined>) {
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) url.searchParams.set(key, String(value));
  }
}

async function daemonRequest(
  endpoint: string,
  options: {
    env: NodeJS.ProcessEnv;
    fetchImpl: Fetch;
    method?: 'GET' | 'POST';
    auth?: boolean;
    body?: unknown;
    params?: Record<string, string | number | undefined>;
  },
): Promise<unknown> {
  const url = new URL(`${daemonBaseUrl(options.env)}${endpoint}`);
  appendParams(url, options.params ?? {});

  const headers: Record<string, string> = {};
  if (options.auth !== false) {
    headers.Authorization = `Bearer ${authToken(options.env)}`;
  }
  if (options.body !== undefined) {
    headers['Content-Type'] = 'application/json';
  }

  const response = await options.fetchImpl(url, {
    method: options.method ?? (options.body === undefined ? 'GET' : 'POST'),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });

  const text = await response.text();
  let payload: unknown = null;
  if (text.trim()) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = text;
    }
  }

  if (!response.ok) {
    throw new Error(`daemon HTTP ${response.status}: ${redactSensitive(text, options.env)}`);
  }

  return payload;
}

export const daemonTools = [
  describeTool('daemon_health', 'Check the local Covenant daemon HTTP gateway health.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('daemon_tools_list', 'List tools registered in the local Covenant daemon.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('daemon_memory_recent', 'Read recent Covenant memory records visible to the authenticated peer.', {
    type: 'object',
    properties: {
      tier: { type: 'string', enum: ['working', 'episodic', 'longterm'] },
      limit: { type: 'number' },
    },
  }),
  describeTool('daemon_memory_search', 'Search Covenant memory visible to the authenticated peer.', {
    type: 'object',
    properties: {
      q: { type: 'string' },
      tier: { type: 'string', enum: ['working', 'episodic', 'longterm'] },
      limit: { type: 'number' },
    },
    required: ['q'],
  }),
  describeTool('daemon_audit_recent', 'Read recent Covenant audit events visible to the authenticated peer.', {
    type: 'object',
    properties: {
      limit: { type: 'number' },
    },
  }),
  describeTool('daemon_receipts_recent', 'Read recent Covenant settlement receipts for the authenticated peer.', {
    type: 'object',
    properties: {
      limit: { type: 'number' },
    },
  }),
  describeTool('daemon_a2a_status', 'Inspect queued, in-flight, and pending Covenant A2A work.', {
    type: 'object',
    properties: {
      limit: { type: 'number' },
    },
  }),
  describeTool('daemon_budget_debits', 'Read recent Covenant budget debit events.', {
    type: 'object',
    properties: {
      limit: { type: 'number' },
    },
  }),
  describeTool('daemon_chain_status', 'Read Covenant Solana chain configuration status from the daemon.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('daemon_submit_intent', 'Submit a natural-language intent through the Covenant daemon.', {
    type: 'object',
    properties: {
      text: { type: 'string' },
    },
    required: ['text'],
  }),
  describeTool('daemon_tools_call', 'Call a named Covenant daemon tool through the capability-gated daemon surface.', {
    type: 'object',
    properties: {
      name: { type: 'string' },
      arguments: { type: 'object' },
    },
    required: ['name'],
  }),
  describeTool('daemon_a2a_send_task', 'Queue a Covenant A2A task through the daemon.', {
    type: 'object',
    properties: {
      task: { type: 'object' },
    },
    required: ['task'],
  }),
  describeTool('daemon_a2a_post_result', 'Post a Covenant A2A task result through the daemon.', {
    type: 'object',
    properties: {
      result: { type: 'object' },
    },
    required: ['result'],
  }),
  describeTool('daemon_capabilities_grant', 'Grant a Covenant capability to the authenticated peer.', {
    type: 'object',
    properties: {
      action: { type: 'string' },
      scope: { type: 'object' },
      expires_at: { type: 'number' },
    },
    required: ['action'],
  }),
  describeTool('daemon_capabilities_revoke', 'Revoke a Covenant capability by signature.', {
    type: 'object',
    properties: {
      signature_b58: { type: 'string' },
    },
    required: ['signature_b58'],
  }),
  describeTool('daemon_flush_receipts', 'Batch local Covenant settlement receipts through the daemon.', {
    type: 'object',
    properties: {
      limit: { type: 'number' },
    },
  }),
];

export async function callDaemonTool(
  name: string,
  args: unknown,
  deps: { env?: NodeJS.ProcessEnv; fetchImpl?: Fetch } = {},
): Promise<ToolResult | null> {
  const env = deps.env ?? process.env;
  const fetchImpl = deps.fetchImpl ?? fetch;

  try {
    switch (name) {
      case 'daemon_health':
        return asText(await daemonRequest('/health', { env, fetchImpl, auth: false }));
      case 'daemon_tools_list':
        return asText(await daemonRequest('/tools', { env, fetchImpl }));
      case 'daemon_memory_recent': {
        const parsed = memoryRecentSchema.parse(args);
        return asText(
          await daemonRequest('/memory/recent', {
            env,
            fetchImpl,
            params: { tier: parsed.tier, limit: parsed.limit },
          }),
        );
      }
      case 'daemon_memory_search': {
        const parsed = memorySearchSchema.parse(args);
        return asText(
          await daemonRequest('/memory/search', {
            env,
            fetchImpl,
            params: { q: parsed.q, tier: parsed.tier, limit: parsed.limit },
          }),
        );
      }
      case 'daemon_audit_recent': {
        const parsed = limitSchema.parse(args);
        return asText(await daemonRequest('/audit/recent', { env, fetchImpl, params: parsed }));
      }
      case 'daemon_receipts_recent': {
        const parsed = limitSchema.parse(args);
        return asText(await daemonRequest('/receipts/recent', { env, fetchImpl, params: parsed }));
      }
      case 'daemon_a2a_status': {
        const parsed = limitSchema.parse(args);
        return asText(await daemonRequest('/a2a/queue', { env, fetchImpl, params: parsed }));
      }
      case 'daemon_budget_debits': {
        const parsed = limitSchema.parse(args);
        return asText(await daemonRequest('/budget/debits', { env, fetchImpl, params: parsed }));
      }
      case 'daemon_chain_status':
        return asText(await daemonRequest('/chain/status', { env, fetchImpl }));
      case 'daemon_submit_intent': {
        const parsed = z.object({ text: z.string().min(1) }).parse(args);
        return asText(await daemonRequest('/intent', { env, fetchImpl, body: parsed }));
      }
      case 'daemon_tools_call': {
        const parsed = toolCallSchema.parse(args);
        return asText(
          await daemonRequest('/tools/call', {
            env,
            fetchImpl,
            body: { name: parsed.name, arguments: parsed.arguments ?? {} },
          }),
        );
      }
      case 'daemon_a2a_send_task': {
        const parsed = z.object({ task: looseObjectSchema }).parse(args);
        return asText(await daemonRequest('/a2a/tasks', { env, fetchImpl, body: parsed.task }));
      }
      case 'daemon_a2a_post_result': {
        const parsed = z.object({ result: looseObjectSchema }).parse(args);
        return asText(await daemonRequest('/a2a/results', { env, fetchImpl, body: parsed.result }));
      }
      case 'daemon_capabilities_grant': {
        const parsed = grantCapabilitySchema.parse(args);
        return asText(await daemonRequest('/capabilities/grant', { env, fetchImpl, body: parsed }));
      }
      case 'daemon_capabilities_revoke': {
        const parsed = revokeCapabilitySchema.parse(args);
        return asText(await daemonRequest('/capabilities/revoke', { env, fetchImpl, body: parsed }));
      }
      case 'daemon_flush_receipts': {
        const parsed = limitSchema.parse(args);
        return asText(await daemonRequest('/chain/flush-receipts', { env, fetchImpl, body: parsed }));
      }
      default:
        return null;
    }
  } catch (error) {
    if (error instanceof z.ZodError) {
      return asError(error.issues.map((issue) => issue.message).join('; '));
    }
    if (error instanceof Error) {
      return asError(redactSensitive(error.message, env));
    }
    return asError('Unknown daemon bridge failure');
  }
}
