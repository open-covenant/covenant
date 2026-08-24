import { PassThrough } from 'node:stream';
import type { IncomingMessage } from 'node:http';
import { describe, expect, it } from 'vitest';
import { readBody } from '../src/server.js';

describe('request body limit', () => {
  it('reads a bounded request body', async () => {
    const request = stream();
    const body = readBody(request, 16);
    request.end('bounded');
    await expect(body).resolves.toBe('bounded');
  });

  it('rejects a streaming body as soon as it crosses the cap', async () => {
    const request = stream();
    const body = readBody(request, 8);
    request.write('12345678');
    request.end('9');
    await expect(body).rejects.toThrow(/body exceeds/);
  });
});

function stream(): PassThrough & IncomingMessage {
  const request = new PassThrough() as PassThrough & IncomingMessage;
  request.headers = {};
  return request;
}
