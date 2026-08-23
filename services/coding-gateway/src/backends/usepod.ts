import { config } from '../config.js';
import type {
  CodingBackend,
  GatewayEvent,
  ProviderReceipt,
  Sandbox,
  TokenUsage,
} from '../types.js';
import {
  accountUsePodTurn,
  boundedMaxTokens,
  parseUsePodUsage,
  providerReceipt,
  usePodHeaders,
  usePodUrl,
  type UsePodRequestConfig,
} from '../usepod-http.js';

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
    private readonly baseUrl = config.usePodBaseUrl,
    private readonly apiKey = process.env.USEPOD_API_KEY ?? '',
    private readonly model = config.model,
    private readonly maxInputPriceMicrounits = config.usePodMaxInputPriceMicrounits,
    private readonly maxOutputPriceMicrounits = config.usePodMaxOutputPriceMicrounits,
    private readonly minimumBalance = config.usePodMinimumBalance,
  ) {}

  async run(opts: {
    input: string;
    sandbox: Sandbox;
    signal: AbortSignal;
    emit: (event: GatewayEvent) => void;
    maxProviderCostUsd: number;
    recordProviderRequest?: () => void | Promise<void>;
    recordProviderReceipt?: (receipt: ProviderReceipt) => void | Promise<void>;
  }): Promise<{ output: string; usage: TokenUsage; providerReceipts: ProviderReceipt[] }> {
    if (!this.apiKey) throw new Error('USEPOD_API_KEY is required for the UsePod backend');

    const requestConfig: UsePodRequestConfig = {
      baseUrl: this.baseUrl,
      token: this.apiKey,
      model: this.model,
      maxInputPriceMicrounits: this.maxInputPriceMicrounits,
      maxOutputPriceMicrounits: this.maxOutputPriceMicrounits,
      minimumBalance: this.minimumBalance,
    };
    const budgetMicrounits = Math.floor(opts.maxProviderCostUsd * 1_000_000);
    if (!Number.isSafeInteger(budgetMicrounits) || budgetMicrounits <= 0) {
      throw new Error('UsePod provider spend budget must be positive');
    }

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
    const providerReceipts: ProviderReceipt[] = [];
    let spentMicrounits = 0n;

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      if (opts.signal.aborted) throw new Error('run aborted');
      const remainingMicrounits = BigInt(budgetMicrounits) - spentMicrounits;
      if (remainingMicrounits <= 0n) throw new Error('UsePod provider spend budget exhausted');
      const draft = {
        model: this.model,
        messages,
        tools: TOOLS,
        max_tokens: MAX_OUTPUT_TOKENS,
        temperature: 0.1,
      };
      const maxTokens = boundedMaxTokens(
        draft,
        Number(remainingMicrounits),
        this.maxInputPriceMicrounits,
        this.maxOutputPriceMicrounits,
        MAX_OUTPUT_TOKENS,
      );
      await opts.recordProviderRequest?.();
      const response = await fetch(usePodUrl(requestConfig, 'chat/completions'), {
        method: 'POST',
        signal: opts.signal,
        headers: usePodHeaders(requestConfig),
        body: JSON.stringify({ ...draft, max_tokens: maxTokens }),
      });
      const routeReceipt = providerReceipt(response, this.model, requestConfig.minimumBalance);
      if (!response.ok) {
        const body = await response.text();
        throw new Error(`UsePod HTTP ${response.status}: ${body.slice(0, 1_000)}`);
      }

      const body = (await response.json()) as {
        model?: unknown;
        choices?: Array<{
          message?: { content?: string | null; tool_calls?: ToolCall[] };
        }>;
        usage?: unknown;
      };
      if (body.model !== this.model) throw new Error('UsePod returned a different model');
      const turnUsage = parseUsePodUsage(body.usage);
      const receipt = accountUsePodTurn(
        routeReceipt,
        turnUsage,
        this.maxInputPriceMicrounits,
        this.maxOutputPriceMicrounits,
      );
      providerReceipts.push(receipt);
      await opts.recordProviderReceipt?.(receipt);
      spentMicrounits += BigInt(receipt.accounting.accountedCostMicrounits);
      if (spentMicrounits > BigInt(budgetMicrounits)) {
        throw new Error('UsePod provider spend exceeded the run budget');
      }
      const message = body.choices?.[0]?.message;
      if (!message) throw new Error('UsePod returned no completion choice');
      usage.inputTokens = addTokens(usage.inputTokens, turnUsage.promptTokens);
      usage.outputTokens = addTokens(usage.outputTokens, turnUsage.completionTokens);
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
        return { output, usage, providerReceipts };
      }
      if (spentMicrounits >= BigInt(budgetMicrounits)) {
        throw new Error('UsePod provider spend budget exhausted before the next turn');
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

function addTokens(total: number, increment: number): number {
  const next = total + increment;
  if (!Number.isSafeInteger(next)) throw new Error('UsePod cumulative token usage overflowed');
  return next;
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
