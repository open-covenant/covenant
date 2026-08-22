import { createHmac, timingSafeEqual } from 'node:crypto';
import { z } from 'zod';
import type { MizukiStore } from './store.js';

const pullRequestSchema = z.object({
  action: z.string(),
  pull_request: z.object({
    html_url: z.string().url(),
    merged: z.boolean(),
    merged_at: z.string().nullable(),
  }),
});

export class GithubWebhookHandler {
  constructor(
    private readonly store: MizukiStore,
    private readonly onPullRequest: (
      payload: z.infer<typeof pullRequestSchema>,
    ) => Promise<void> = async () => {},
  ) {}

  async handle(deliveryId: string, event: string, rawBody: Buffer): Promise<boolean> {
    const payload =
      event === 'pull_request'
        ? pullRequestSchema.parse(JSON.parse(rawBody.toString('utf8')))
        : undefined;
    const lease = await this.store.beginWebhookDelivery(deliveryId);
    if (lease.state !== 'started') return false;
    try {
      if (payload) {
        await this.onPullRequest(payload);
      }
      await this.store.completeWebhookDelivery(deliveryId, lease.leaseId);
      return true;
    } catch (error) {
      await this.store.failWebhookDelivery(
        deliveryId,
        lease.leaseId,
        error instanceof Error ? error.message : 'unknown webhook processing error',
      );
      throw error;
    }
  }
}

export function verifyGithubWebhook(secret: string, rawBody: Buffer, signature: string): boolean {
  if (!signature.startsWith('sha256=')) return false;
  const supplied = signature.slice(7);
  const expected = createHmac('sha256', secret).update(rawBody).digest('hex');
  if (supplied.length !== expected.length) return false;
  return timingSafeEqual(Buffer.from(supplied), Buffer.from(expected));
}
