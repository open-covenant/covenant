import { describe, expect, it } from 'vitest';
import { matchesUsePodModel, readUsePodCompletion } from './usepod-response.js';

const MODEL = 'deepseek-v4-flash-0731';

function chunk(
  content: string | null,
  finishReason: 'stop' | null = null,
  model = MODEL,
  id = 'request-1',
): string {
  return JSON.stringify({
    id,
    model,
    choices: [
      {
        index: 0,
        delta: { content },
        finish_reason: finishReason,
      },
    ],
    usage: null,
  });
}

function usage(
  model = MODEL,
  value: unknown = { prompt_tokens: 96, completion_tokens: 63 },
): string {
  return JSON.stringify({ model, choices: [], usage: value });
}

function stream(frames: string[], lineEnding = '\n'): Response {
  return new Response(frames.map((frame) => `data: ${frame}${lineEnding}${lineEnding}`).join(''), {
    headers: { 'content-type': 'text/event-stream' },
  });
}

describe('UsePod completion responses', () => {
  it('accepts the existing JSON completion shape', async () => {
    const content = JSON.stringify({ approved: true, reason: 'scoped' });
    await expect(
      readUsePodCompletion(
        Response.json({
          id: 'request-1',
          model: 'review-model',
          choices: [
            {
              index: 0,
              finish_reason: 'stop',
              message: { role: 'assistant', content },
            },
          ],
          usage: { prompt_tokens: 12, completion_tokens: 8 },
        }),
      ),
    ).resolves.toEqual({ model: 'review-model', content });
  });

  it.each([
    {
      choices: [{ index: 0, message: { content: '{}' } }],
      usage: { prompt_tokens: 1, completion_tokens: 1 },
    },
    {
      choices: [
        {
          index: 0,
          finish_reason: 'stop',
          message: { content: '{}', refusal: 'cannot review' },
        },
      ],
      usage: { prompt_tokens: 1, completion_tokens: 1 },
    },
    {
      choices: [
        {
          index: 0,
          finish_reason: 'stop',
          message: { content: '{}', tool_calls: [{ id: 'tool-1' }] },
        },
      ],
      usage: { prompt_tokens: 1, completion_tokens: 1 },
    },
    {
      choices: [
        {
          index: 0,
          finish_reason: 'stop',
          message: { content: '{}' },
        },
      ],
    },
  ])('rejects an incomplete or unsafe JSON completion %#', async ({ choices, usage }) => {
    await expect(
      readUsePodCompletion(
        Response.json({ model: 'review-model', choices, ...(usage ? { usage } : {}) }),
      ),
    ).rejects.toThrow();
  });

  it('assembles the live streamed shape with CRLF, comments, and a qualified alias', async () => {
    const response = stream(
      [chunk('{"approved":'), chunk('true,"reason":"scoped"}', 'stop'), usage(), '[DONE]'],
      '\r\n',
    );
    const body = `\uFEFF  : keepalive\r\n\r\n${await response.text()}`;
    await expect(readUsePodCompletion(new Response(body))).resolves.toEqual({
      model: MODEL,
      content: '{"approved":true,"reason":"scoped"}',
    });
    expect(matchesUsePodModel('deepseek-v4-flash', MODEL)).toBe(true);
  });

  it.each([
    {
      name: 'missing terminator',
      frames: [chunk('{}', 'stop'), usage()],
    },
    {
      name: 'duplicate terminator',
      frames: [chunk('{}', 'stop'), usage(), '[DONE]', '[DONE]'],
    },
    {
      name: 'data after terminator',
      frames: [chunk('{}', 'stop'), usage(), '[DONE]', usage()],
    },
    {
      name: 'model drift',
      frames: [chunk('{}', 'stop'), usage('different-model'), '[DONE]'],
    },
    {
      name: 'request identity drift',
      frames: [
        chunk('{', null, MODEL, 'request-1'),
        chunk('}', 'stop', MODEL, 'request-2'),
        usage(),
        '[DONE]',
      ],
    },
    {
      name: 'usage before completion',
      frames: [usage(), chunk('{}', 'stop'), '[DONE]'],
    },
    {
      name: 'invalid usage',
      frames: [
        chunk('{}', 'stop'),
        usage(MODEL, { prompt_tokens: -1, completion_tokens: 1 }),
        '[DONE]',
      ],
    },
  ])('rejects $name', async ({ frames }) => {
    await expect(readUsePodCompletion(stream(frames))).rejects.toThrow();
  });

  it('rejects tool calls and an unsupported response body without exposing either', async () => {
    const toolCall = JSON.stringify({
      id: 'request-1',
      model: MODEL,
      choices: [
        {
          index: 0,
          delta: { tool_calls: [{ id: 'private-tool-call' }] },
          finish_reason: 'stop',
        },
      ],
      usage: null,
    });
    await expect(readUsePodCompletion(stream([toolCall, usage(), '[DONE]']))).rejects.not.toThrow(
      'private-tool-call',
    );
    await expect(
      readUsePodCompletion(new Response('private-provider-diagnostic')),
    ).rejects.not.toThrow('private-provider-diagnostic');
  });

  it('accepts only explicit resolved model identities', () => {
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek-v4-flash-260425')).toBe(true);
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek/deepseek-v4-flash-0731')).toBe(true);
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek-v4-flash-latest')).toBe(false);
    expect(matchesUsePodModel('review-model', 'review-model')).toBe(true);
    expect(matchesUsePodModel('review model', 'review model')).toBe(false);
  });
});
