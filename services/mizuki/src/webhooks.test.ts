import { createHmac } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { MemoryStore } from './store.js';
import { GithubWebhookHandler, verifyGithubWebhook } from './webhooks.js';

describe('GitHub webhooks', () => {
  it('verifies the exact raw payload', () => {
    const secret = 'webhook-secret';
    const body = Buffer.from('{"action":"closed"}');
    const signature = `sha256=${createHmac('sha256', secret).update(body).digest('hex')}`;
    expect(verifyGithubWebhook(secret, body, signature)).toBe(true);
    expect(verifyGithubWebhook(secret, Buffer.from('{}'), signature)).toBe(false);
  });

  it('processes a delivery once', async () => {
    const store = new MemoryStore();
    const callback = vi.fn(async () => {});
    const handler = new GithubWebhookHandler(store, callback);
    const body = Buffer.from(
      JSON.stringify({
        action: 'closed',
        pull_request: {
          html_url: 'https://github.com/example/project/pull/1',
          merged: true,
          merged_at: '2026-08-22T12:00:00Z',
        },
      }),
    );
    expect(await handler.handle('delivery-1', 'pull_request', body)).toBe(true);
    expect(await handler.handle('delivery-1', 'pull_request', body)).toBe(false);
    expect(callback).toHaveBeenCalledOnce();
  });

  it('retries a delivery when its first side effect fails', async () => {
    const store = new MemoryStore();
    const callback = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error('temporary failure'))
      .mockResolvedValue(undefined);
    const handler = new GithubWebhookHandler(store, callback);
    const body = Buffer.from(
      JSON.stringify({
        action: 'closed',
        pull_request: {
          html_url: 'https://github.com/example/project/pull/1',
          merged: true,
          merged_at: '2026-08-22T12:00:00Z',
        },
      }),
    );

    await expect(handler.handle('delivery-retry', 'pull_request', body)).rejects.toThrow(
      'temporary failure',
    );
    expect(await handler.handle('delivery-retry', 'pull_request', body)).toBe(true);
    expect(await handler.handle('delivery-retry', 'pull_request', body)).toBe(false);
    expect(callback).toHaveBeenCalledTimes(2);
  });
});
