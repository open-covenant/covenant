// Rasterization allocates native memory as well as JavaScript objects. A burst
// of crawlers must never run several rasterizers or retain unbounded results
// inside the same process that serves the public website.
export class CardQueueFull extends Error {}

type CachedCard = { body: ArrayBuffer; expiresAt: number };
type Job = { key: string; render: () => Promise<ArrayBuffer>; resolve: (body: ArrayBuffer) => void; reject: (reason: unknown) => void };

export class CardPipeline {
  private readonly cache = new Map<string, CachedCard>();
  private readonly pending = new Map<string, Promise<ArrayBuffer>>();
  private readonly queue: Job[] = [];
  private cachedBytes = 0;
  private active = false;

  constructor(private readonly options = { maxPending: 8, maxEntries: 64, maxBytes: 8 * 1024 * 1024, ttlMs: 6 * 60 * 60 * 1000 }) {}

  getOrRender(key: string, render: () => Promise<ArrayBuffer>): Promise<ArrayBuffer> {
    const hit = this.cache.get(key);
    if (hit) {
      this.cache.delete(key);
      if (hit.expiresAt > Date.now()) {
        this.cache.set(key, hit);
        return Promise.resolve(hit.body);
      }
      this.cachedBytes -= hit.body.byteLength;
    }
    const pending = this.pending.get(key);
    if (pending) return pending;
    if (this.pending.size >= this.options.maxPending) return Promise.reject(new CardQueueFull("Card renderer is busy"));

    const promise = new Promise<ArrayBuffer>((resolve, reject) => {
      this.queue.push({ key, render, resolve, reject });
    });
    this.pending.set(key, promise);
    void this.run();
    return promise;
  }

  private remember(key: string, body: ArrayBuffer) {
    if (body.byteLength > this.options.maxBytes) return;
    while (this.cache.size >= this.options.maxEntries || this.cachedBytes + body.byteLength > this.options.maxBytes) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cachedBytes -= this.cache.get(oldest)!.body.byteLength;
      this.cache.delete(oldest);
    }
    this.cache.set(key, { body, expiresAt: Date.now() + this.options.ttlMs });
    this.cachedBytes += body.byteLength;
  }

  private async run() {
    if (this.active) return;
    this.active = true;
    try {
      while (this.queue.length) {
        const job = this.queue.shift()!;
        try {
          const body = await job.render();
          this.remember(job.key, body);
          job.resolve(body);
        } catch (error) {
          job.reject(error);
        } finally {
          this.pending.delete(job.key);
        }
      }
    } finally {
      this.active = false;
    }
  }
}
