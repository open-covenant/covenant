import { z } from 'zod';

const MAX_RESPONSE_BYTES = 65_536;
const MAX_STREAM_FRAMES = 1_024;
const RESOLVED_MODELS: ReadonlyMap<string, ReadonlySet<string>> = new Map([
  [
    'deepseek-v4-flash',
    new Set([
      'deepseek-v4-flash-0731',
      'deepseek-v4-flash-260425',
      'deepseek/deepseek-v4-flash-0731',
    ]),
  ],
]);

const modelSchema = z
  .string()
  .min(1)
  .max(256)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/);
const requestIdSchema = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/);
const usageSchema = z
  .object({
    prompt_tokens: z.number().int().nonnegative(),
    completion_tokens: z.number().int().nonnegative(),
  })
  .passthrough();
const jsonCompletionSchema = z
  .object({
    id: requestIdSchema.optional(),
    model: modelSchema,
    choices: z
      .array(
        z
          .object({
            index: z.literal(0),
            finish_reason: z.literal('stop'),
            message: z
              .object({
                role: z.literal('assistant').optional(),
                content: z.string().min(1),
                reasoning_content: z.string().nullable().optional(),
              })
              .strict(),
          })
          .passthrough(),
      )
      .length(1),
    usage: usageSchema,
  })
  .passthrough()
  .refine((value) => !Object.hasOwn(value, 'error'));
const streamChoiceSchema = z
  .object({
    index: z.literal(0),
    finish_reason: z.union([z.literal('stop'), z.null()]).optional(),
    delta: z
      .object({
        role: z.literal('assistant').optional(),
        content: z.string().nullable().optional(),
        reasoning_content: z.string().nullable().optional(),
      })
      .strict(),
  })
  .passthrough();
const streamChunkSchema = z
  .object({
    id: requestIdSchema.optional(),
    model: modelSchema,
    choices: z.array(streamChoiceSchema).max(1),
    usage: z.unknown().optional(),
  })
  .passthrough()
  .refine((value) => !Object.hasOwn(value, 'error'));

export interface UsePodCompletion {
  model: string;
  content: string;
}

export async function readUsePodCompletion(response: Response): Promise<UsePodCompletion> {
  const text = await boundedText(response);
  const body = normalizeStart(text);
  if (body.startsWith('{')) return parseJsonCompletion(body);
  if (body.startsWith('data:') || body.startsWith(':')) return parseEventStream(body);
  throw new Error('unsupported completion response');
}

export function matchesUsePodModel(requested: string, returned: string): boolean {
  if (!modelSchema.safeParse(requested).success || !modelSchema.safeParse(returned).success) {
    return false;
  }
  if (requested === returned) return true;
  return RESOLVED_MODELS.get(requested)?.has(returned) ?? false;
}

async function boundedText(response: Response): Promise<string> {
  const length = response.headers.get('content-length')?.trim();
  if (length && (!/^\d+$/.test(length) || BigInt(length) > BigInt(MAX_RESPONSE_BYTES))) {
    throw new Error('completion response exceeded its size limit');
  }
  if (!response.body) throw new Error('completion response was empty');

  const reader = response.body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const chunks: string[] = [];
  let received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (received > MAX_RESPONSE_BYTES) {
        await reader.cancel();
        throw new Error('completion response exceeded its size limit');
      }
      chunks.push(decoder.decode(value, { stream: true }));
    }
    chunks.push(decoder.decode());
    return chunks.join('');
  } finally {
    reader.releaseLock();
  }
}

function parseJsonCompletion(text: string): UsePodCompletion {
  let value: unknown;
  try {
    value = JSON.parse(text) as unknown;
  } catch {
    throw new Error('completion response was malformed');
  }
  const parsed = jsonCompletionSchema.safeParse(value);
  if (!parsed.success) throw new Error('completion response was invalid');
  return {
    model: parsed.data.model,
    content: parsed.data.choices[0]!.message.content,
  };
}

function parseEventStream(text: string): UsePodCompletion {
  const frames = eventData(text);
  let model: string | undefined;
  let requestId: string | undefined;
  let requestIdPresent: boolean | undefined;
  let finished = false;
  let terminated = false;
  let usageSeen = false;
  let usage: unknown;
  const content: string[] = [];

  for (const data of frames) {
    if (data === '[DONE]') {
      if (terminated) throw new Error('completion stream had duplicate terminators');
      terminated = true;
      continue;
    }
    if (terminated) throw new Error('completion stream continued after its terminator');

    let value: unknown;
    try {
      value = JSON.parse(data) as unknown;
    } catch {
      throw new Error('completion stream contained malformed JSON');
    }
    const chunk = streamChunkSchema.safeParse(value);
    if (!chunk.success) throw new Error('completion stream contained an invalid chunk');
    if (model && model !== chunk.data.model) {
      throw new Error('completion stream changed model identity');
    }
    model ??= chunk.data.model;

    const hasRequestId = chunk.data.id !== undefined;
    if (chunk.data.choices.length > 0) {
      requestIdPresent ??= hasRequestId;
      if (requestIdPresent !== hasRequestId) {
        throw new Error('completion stream changed request identity');
      }
    }
    if (requestId && chunk.data.id && requestId !== chunk.data.id) {
      throw new Error('completion stream changed request identity');
    }
    requestId ??= chunk.data.id;

    const hasUsage = chunk.data.usage !== undefined && chunk.data.usage !== null;
    if (hasUsage) {
      if (usageSeen && !sameJsonValue(usage, chunk.data.usage)) {
        throw new Error('completion stream contained conflicting usage');
      }
      usage = chunk.data.usage;
      usageSeen = true;
    }

    if (chunk.data.choices.length === 0) {
      if (!hasUsage || !finished) throw new Error('completion stream had an invalid usage chunk');
      continue;
    }
    if (hasUsage) throw new Error('completion stream had an invalid usage chunk');
    if (finished) throw new Error('completion stream continued after completion');

    const choice = chunk.data.choices[0]!;
    if (choice.delta.content) content.push(choice.delta.content);
    if (choice.finish_reason === 'stop') finished = true;
  }

  if (!terminated || !finished || !usageSeen || !model || !validUsage(usage)) {
    throw new Error('completion stream ended before completion');
  }
  const joined = content.join('');
  if (!joined.trim()) throw new Error('completion stream returned empty content');
  return { model, content: joined };
}

function eventData(text: string): string[] {
  const frames: string[] = [];
  let dataLines: string[] = [];
  const flush = () => {
    if (dataLines.length > 0) frames.push(dataLines.join('\n'));
    dataLines = [];
  };

  for (const line of text.split(/\r\n|\r|\n/)) {
    if (line === '') {
      flush();
      continue;
    }
    if (line.startsWith(':')) continue;
    if (!line.startsWith('data:')) throw new Error('completion stream had an unsupported field');
    const value = line.slice(5);
    dataLines.push(value.startsWith(' ') ? value.slice(1) : value);
    if (frames.length + 1 > MAX_STREAM_FRAMES) {
      throw new Error('completion stream had too many frames');
    }
  }
  if (dataLines.length > 0) flush();
  if (frames.length === 0 || frames.length > MAX_STREAM_FRAMES) {
    throw new Error('completion stream had no frames');
  }
  return frames;
}

function validUsage(value: unknown): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const usage = value as Record<string, unknown>;
  return [usage.prompt_tokens, usage.completion_tokens].every(
    (count) => typeof count === 'number' && Number.isSafeInteger(count) && count >= 0,
  );
}

function sameJsonValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => sameJsonValue(value, right[index]))
    );
  }
  if (!left || !right || typeof left !== 'object' || typeof right !== 'object') return false;
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const leftKeys = Object.keys(leftRecord).sort();
  const rightKeys = Object.keys(rightRecord).sort();
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key, index) => key === rightKeys[index] && sameJsonValue(leftRecord[key], rightRecord[key]),
    )
  );
}

function normalizeStart(value: string): string {
  return value
    .trimStart()
    .replace(/^\uFEFF/, '')
    .trimStart();
}
