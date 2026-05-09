import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js';
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

type Fetch = typeof fetch;
type ToolInputSchema = Tool['inputSchema'];

const DEFAULT_HERMES_API_BASE_URL = 'http://127.0.0.1:8642/v1';

const runSchema = z.object({
  input: z.string().min(1).optional(),
  prompt: z.string().min(1).optional(),
  session_id: z.string().min(1).optional(),
  instructions: z.string().min(1).optional(),
  previous_response_id: z.string().min(1).optional(),
  conversation_history: z.array(z.record(z.string(), z.unknown())).optional(),
})
  .refine((value) => value.input || value.prompt, 'expected input or prompt')
  .transform(({ prompt, ...value }) => ({
    ...value,
    input: value.input ?? prompt,
  }));

const runIdSchema = z.object({
  run_id: z.string().min(1),
});

const eventsSchema = runIdSchema.extend({
  cursor: z.string().min(1).optional(),
  limit: z.number().int().positive().max(500).optional(),
});

function describeTool(name: string, description: string, inputSchema: ToolInputSchema): Tool {
  return { name, description, inputSchema };
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
    content: [{ type: 'text', text: message }],
    isError: true,
  };
}

function baseUrl(env: NodeJS.ProcessEnv): string {
  return (env.HERMES_API_BASE_URL ?? DEFAULT_HERMES_API_BASE_URL).replace(/\/+$/, '');
}

function redactSensitive(input: string, env: NodeJS.ProcessEnv): string {
  let out = input;
  for (const [value, label] of [
    [env.HERMES_API_KEY, '<redacted-token>'],
    [env.HERMES_API_BASE_URL, '$HERMES_API_BASE_URL'],
    [env.HOME, '$HOME'],
  ] as const) {
    if (value) out = out.split(value).join(label);
  }
  return out;
}

function appendParams(url: URL, params: Record<string, string | number | undefined>) {
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) url.searchParams.set(key, String(value));
  }
}

async function hermesRequest(
  endpoint: string,
  options: {
    env: NodeJS.ProcessEnv;
    fetchImpl: Fetch;
    method?: 'GET' | 'POST';
    body?: unknown;
    params?: Record<string, string | number | undefined>;
  },
): Promise<unknown> {
  const url = new URL(`${baseUrl(options.env)}${endpoint}`);
  appendParams(url, options.params ?? {});

  const headers: Record<string, string> = {};
  const apiKey = options.env.HERMES_API_KEY?.trim();
  if (apiKey) headers.Authorization = `Bearer ${apiKey}`;
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';

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
    throw new Error(`Hermes API ${response.status}: ${redactSensitive(text, options.env)}`);
  }

  return payload;
}

export const hermesTools = [
  describeTool('hermes_health', 'Check Hermes API Server health.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('hermes_capabilities', 'List Hermes API Server capabilities.', {
    type: 'object',
    properties: {},
    additionalProperties: false,
  }),
  describeTool('hermes_run', 'Start a generic Hermes agent run.', {
    type: 'object',
    properties: {
      input: { type: 'string' },
      prompt: { type: 'string' },
      session_id: { type: 'string' },
      instructions: { type: 'string' },
      previous_response_id: { type: 'string' },
      conversation_history: { type: 'array' },
    },
    anyOf: [{ required: ['input'] }, { required: ['prompt'] }],
  }),
  describeTool('hermes_run_status', 'Read a Hermes agent run status.', {
    type: 'object',
    properties: {
      run_id: { type: 'string' },
    },
    required: ['run_id'],
  }),
  describeTool('hermes_run_events', 'Read Hermes agent run events.', {
    type: 'object',
    properties: {
      run_id: { type: 'string' },
      cursor: { type: 'string' },
      limit: { type: 'number' },
    },
    required: ['run_id'],
  }),
  describeTool('hermes_stop', 'Stop a Hermes agent run.', {
    type: 'object',
    properties: {
      run_id: { type: 'string' },
    },
    required: ['run_id'],
  }),
];

export async function callHermesTool(
  name: string,
  args: unknown,
  deps: { env?: NodeJS.ProcessEnv; fetchImpl?: Fetch } = {},
): Promise<ToolResult | null> {
  const env = deps.env ?? process.env;
  const fetchImpl = deps.fetchImpl ?? fetch;

  try {
    switch (name) {
      case 'hermes_health':
        return asText(await hermesRequest('/health', { env, fetchImpl }));
      case 'hermes_capabilities':
        return asText(await hermesRequest('/capabilities', { env, fetchImpl }));
      case 'hermes_run':
        return asText(
          await hermesRequest('/runs', {
            env,
            fetchImpl,
            body: runSchema.parse(args),
          }),
        );
      case 'hermes_run_status': {
        const { run_id } = runIdSchema.parse(args);
        return asText(
          await hermesRequest(`/runs/${encodeURIComponent(run_id)}`, {
            env,
            fetchImpl,
          }),
        );
      }
      case 'hermes_run_events': {
        const parsed = eventsSchema.parse(args);
        return asText(
          await hermesRequest(`/runs/${encodeURIComponent(parsed.run_id)}/events`, {
            env,
            fetchImpl,
            params: { cursor: parsed.cursor, limit: parsed.limit },
          }),
        );
      }
      case 'hermes_stop': {
        const { run_id } = runIdSchema.parse(args);
        return asText(
          await hermesRequest(`/runs/${encodeURIComponent(run_id)}/stop`, {
            env,
            fetchImpl,
            method: 'POST',
          }),
        );
      }
      default:
        return null;
    }
  } catch (error) {
    if (error instanceof z.ZodError) {
      return asError(error.issues.map((issue) => issue.message).join('; '));
    }
    if (error instanceof Error) return asError(redactSensitive(error.message, env));
    return asError('Unknown Hermes bridge failure');
  }
}

const server = new Server(
  {
    name: 'covenant-hermes-agent',
    version: '0.1.0',
  },
  {
    capabilities: {
      tools: {},
    },
  },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: hermesTools,
}));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  return (
    (await callHermesTool(request.params.name, request.params.arguments ?? {})) ??
    asError(`Unknown tool: ${request.params.name}`)
  );
});

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  const transport = new StdioServerTransport();
  await server.connect(transport);
}
