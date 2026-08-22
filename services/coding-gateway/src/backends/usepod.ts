import type { CodingBackend, GatewayEvent, Sandbox, TokenUsage } from '../types.js';

const MAX_TURNS = 30;
const MAX_OUTPUT_TOKENS = 16_000;

type ToolCall = {
  id: string;
  function: { name: string; arguments: string };
};

type Message = {
  role: 'system' | 'user' | 'assistant' | 'tool';
  content: string | null;
  tool_call_id?: string;
  tool_calls?: ToolCall[];
};

const SYSTEM = `You are Mizuki, an autonomous maintainer working in an ephemeral checkout.
Resolve only the issue described by the user. Inspect before editing, keep the diff small,
run the repository's relevant checks, and stop if the request is ambiguous or unsafe.
Never change workflows, secrets, authentication, cryptography, custody, deployment,
licensing, generated code, or vendored code. Never commit, push, or contact external services.
Finish with a concise summary and the exact validation commands you ran.`;

const TOOLS = [
  {
    type: 'function',
    function: {
      name: 'read_file',
      description: 'Read a UTF-8 file from the repository.',
      parameters: {
        type: 'object',
        properties: { path: { type: 'string' } },
        required: ['path'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'write_file',
      description: 'Create or overwrite a UTF-8 file in the repository.',
      parameters: {
        type: 'object',
        properties: { path: { type: 'string' }, content: { type: 'string' } },
        required: ['path', 'content'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'edit_file',
      description: 'Replace one exact, unique string in a file.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string' },
          old_string: { type: 'string' },
          new_string: { type: 'string' },
        },
        required: ['path', 'old_string', 'new_string'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'bash',
      description: 'Run a bounded shell command in the repository.',
      parameters: {
        type: 'object',
        properties: { command: { type: 'string' } },
        required: ['command'],
      },
    },
  },
] as const;

export class UsePodBackend implements CodingBackend {
  readonly id = 'usepod' as const;

  constructor(
    private readonly baseUrl = process.env.USEPOD_BASE_URL ?? 'https://api.usepod.ai/v1',
    private readonly apiKey = process.env.USEPOD_API_KEY ?? '',
    private readonly model = process.env.CODER_MODEL ?? 'claude-sonnet-4-6',
  ) {}

  async run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal;
    emit: (event: GatewayEvent) => void;
  }): Promise<{ output: string; usage: TokenUsage }> {
    if (!this.apiKey) throw new Error('USEPOD_API_KEY is required for the UsePod backend');

    const messages: Message[] = [
      { role: 'system', content: SYSTEM },
      { role: 'user', content: opts.input },
    ];
    const usage: TokenUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
    };
    let output = '';

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      if (opts.signal.aborted) throw new Error('run aborted');
      const response = await fetch(`${this.baseUrl.replace(/\/$/, '')}/chat/completions`, {
        method: 'POST',
        signal: opts.signal,
        headers: {
          authorization: `Bearer ${this.apiKey}`,
          'content-type': 'application/json',
          'x-pod-routing-mode': 'marketplace-only',
          'x-pod-no-retention': 'true',
          ...(process.env.USEPOD_MAX_INPUT_PRICE
            ? { 'x-pod-max-price-input': process.env.USEPOD_MAX_INPUT_PRICE }
            : {}),
          ...(process.env.USEPOD_MAX_OUTPUT_PRICE
            ? { 'x-pod-max-price-output': process.env.USEPOD_MAX_OUTPUT_PRICE }
            : {}),
        },
        body: JSON.stringify({
          model: this.model,
          messages,
          tools: TOOLS,
          max_tokens: MAX_OUTPUT_TOKENS,
          temperature: 0.1,
        }),
      });
      if (!response.ok) {
        const body = await response.text();
        throw new Error(`UsePod HTTP ${response.status}: ${body.slice(0, 1_000)}`);
      }

      const body = (await response.json()) as {
        choices?: Array<{
          message?: { content?: string | null; tool_calls?: ToolCall[] };
        }>;
        usage?: { prompt_tokens?: number; completion_tokens?: number };
      };
      const message = body.choices?.[0]?.message;
      if (!message) throw new Error('UsePod returned no completion choice');
      usage.inputTokens += body.usage?.prompt_tokens ?? 0;
      usage.outputTokens += body.usage?.completion_tokens ?? 0;
      output = message.content ?? output;
      if (message.content) opts.emit({ type: 'message.delta', text: message.content });

      const calls = message.tool_calls ?? [];
      messages.push({
        role: 'assistant',
        content: message.content ?? null,
        ...(calls.length > 0 ? { tool_calls: calls } : {}),
      });
      if (calls.length === 0) {
        opts.emit({ type: 'run.completed', output });
        return { output, usage };
      }

      for (const call of calls) {
        opts.emit({ type: 'tool.started', tool: call.function.name, preview: preview(call) });
        const started = Date.now();
        let result: string;
        let error = false;
        try {
          result = await execute(call, opts.sandbox, opts.emit);
        } catch (cause) {
          error = true;
          result = `error: ${cause instanceof Error ? cause.message : String(cause)}`;
        }
        opts.emit({
          type: 'tool.completed',
          tool: call.function.name,
          duration_s: (Date.now() - started) / 1_000,
          error,
        });
        messages.push({ role: 'tool', tool_call_id: call.id, content: result });
      }
    }

    throw new Error(`UsePod backend exceeded ${MAX_TURNS} turns`);
  }
}

function args(call: ToolCall): Record<string, unknown> {
  try {
    return JSON.parse(call.function.arguments) as Record<string, unknown>;
  } catch {
    throw new Error(`invalid arguments for ${call.function.name}`);
  }
}

function preview(call: ToolCall): string {
  const input = args(call);
  return String(input.path ?? input.command ?? '').slice(0, 120);
}

async function execute(
  call: ToolCall,
  sandbox: Sandbox,
  emit: (event: GatewayEvent) => void,
): Promise<string> {
  const input = args(call);
  switch (call.function.name) {
    case 'read_file':
      return sandbox.readFile(String(input.path));
    case 'write_file': {
      const path = String(input.path);
      const content = String(input.content ?? '');
      await sandbox.writeFile(path, content);
      emit({ type: 'file.written', path, bytes: Buffer.byteLength(content) });
      return `wrote ${path}`;
    }
    case 'edit_file': {
      const path = String(input.path);
      const before = String(input.old_string);
      const after = String(input.new_string);
      const content = await sandbox.readFile(path);
      const first = content.indexOf(before);
      if (first < 0) throw new Error(`old_string not found in ${path}`);
      if (content.indexOf(before, first + before.length) >= 0) {
        throw new Error(`old_string is not unique in ${path}`);
      }
      const updated = content.slice(0, first) + after + content.slice(first + before.length);
      await sandbox.writeFile(path, updated);
      emit({ type: 'file.written', path, bytes: Buffer.byteLength(updated) });
      return `edited ${path}`;
    }
    case 'bash': {
      const result = await sandbox.exec(String(input.command), { timeoutMs: 180_000 });
      return `exit=${result.exitCode}\n${result.stdout}\n${result.stderr}`.slice(0, 32_000);
    }
    default:
      throw new Error(`unknown tool: ${call.function.name}`);
  }
}
