import { describe, expect, it, vi } from "vitest";
import { CardPipeline, CardQueueFull } from "../_pipeline";

const bytes = (length = 4) => new ArrayBuffer(length);
const deferred = () => {
  let resolve!: (body: ArrayBuffer) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<ArrayBuffer>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

describe("bounded card rendering", () => {
  it("runs only one source-read/raster job and shares work for duplicate requests", async () => {
    const pipeline = new CardPipeline();
    const first = deferred();
    const renderFirst = vi.fn(() => first.promise);
    const renderSecond = vi.fn(async () => bytes());
    const a = pipeline.getOrRender("a", renderFirst);
    const duplicate = pipeline.getOrRender("a", renderFirst);
    const b = pipeline.getOrRender("b", renderSecond);
    expect(duplicate).toBe(a);
    expect(renderFirst).toHaveBeenCalledTimes(1);
    expect(renderSecond).not.toHaveBeenCalled();
    first.resolve(bytes());
    await Promise.all([a, b]);
    expect(renderSecond).toHaveBeenCalledTimes(1);
  });

  it("bounds pending jobs while allowing a duplicate to join the existing job", async () => {
    const pipeline = new CardPipeline({ maxPending: 1, maxEntries: 2, maxBytes: 8, ttlMs: 1000 });
    const first = deferred();
    const a = pipeline.getOrRender("a", () => first.promise);
    expect(pipeline.getOrRender("a", () => { throw new Error("must not render twice"); })).toBe(a);
    await expect(pipeline.getOrRender("b", async () => bytes())).rejects.toBeInstanceOf(CardQueueFull);
    first.resolve(bytes());
    await a;
  });

  it("does not cache failures and releases the queue after an error", async () => {
    const pipeline = new CardPipeline();
    const failure = pipeline.getOrRender("a", async () => { throw new Error("source unavailable"); });
    const next = pipeline.getOrRender("b", async () => bytes());
    await expect(failure).rejects.toThrow("source unavailable");
    await expect(next).resolves.toHaveProperty("byteLength", 4);
    const retry = vi.fn(async () => bytes());
    await pipeline.getOrRender("a", retry);
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("limits cached bytes and evicts the least recently used entry", async () => {
    const pipeline = new CardPipeline({ maxPending: 8, maxEntries: 64, maxBytes: 8, ttlMs: 1000 });
    const render = vi.fn(async () => bytes(4));
    await pipeline.getOrRender("a", render);
    await pipeline.getOrRender("b", render);
    await pipeline.getOrRender("a", render);
    await pipeline.getOrRender("c", render);
    expect(render).toHaveBeenCalledTimes(3);
    await pipeline.getOrRender("a", render);
    expect(render).toHaveBeenCalledTimes(3);
    await pipeline.getOrRender("b", render);
    expect(render).toHaveBeenCalledTimes(4);
  });

  it("expires cached results and does not retain a single oversized image", async () => {
    const pipeline = new CardPipeline({ maxPending: 8, maxEntries: 64, maxBytes: 8, ttlMs: 1 });
    const now = vi.spyOn(Date, "now").mockReturnValue(100);
    const render = vi.fn(async () => bytes());
    await pipeline.getOrRender("a", render);
    now.mockReturnValue(102);
    await pipeline.getOrRender("a", render);
    expect(render).toHaveBeenCalledTimes(2);
    now.mockRestore();
    const large = vi.fn(async () => bytes(9));
    await pipeline.getOrRender("large", large);
    await pipeline.getOrRender("large", large);
    expect(large).toHaveBeenCalledTimes(2);
  });
});
