import { describe, it, expect, vi } from 'vitest';
import { createServer } from 'node:http';
import { WebSocketServer } from 'ws';
import { WsTransport, type WebSocketLike } from './wsTransport.js';
import type { InboundFrame, OutboundFrame } from './transport.js';

class FakeSocket implements WebSocketLike {
  static last: FakeSocket | null = null;
  readyState = 0; // CONNECTING
  readonly sent: string[] = [];
  private readonly listeners: Record<string, Array<(ev: unknown) => void>> = {};

  constructor(readonly url: string) {
    FakeSocket.last = this;
  }
  send(data: string): void {
    if (this.readyState !== 1) throw new Error('socket not open');
    this.sent.push(data);
  }
  close(): void {
    this.readyState = 3;
    this.emit('close');
  }
  addEventListener(type: string, listener: (ev: never) => void): void {
    (this.listeners[type] ??= []).push(listener as (ev: unknown) => void);
  }
  private emit(type: string, ev?: unknown): void {
    for (const l of this.listeners[type] ?? []) l(ev);
  }
  // test drivers
  fireOpen(): void {
    this.readyState = 1;
    this.emit('open');
  }
  fireMessage(data: unknown): void {
    this.emit('message', { data });
  }
  fireError(ev: unknown): void {
    this.emit('error', ev);
  }
  onUpgradeRejected(listener: (response: { status: number; retryAfter?: string }) => void): void {
    this.addEventListener('upgrade-rejected', listener);
  }
  fireUpgradeRejected(status: number, retryAfter?: string): void {
    this.emit('upgrade-rejected', { status, retryAfter });
  }
  sentFrames(): OutboundFrame[] {
    return this.sent.map((s) => JSON.parse(s) as OutboundFrame);
  }
}

const factory = (url: string) => new FakeSocket(url);

describe('WsTransport', () => {
  it('announces hello with in-band auth + resume on open, and resolves connect', async () => {
    const t = new WsTransport({
      url: 'wss://mesh',
      token: 'TKN',
      agentId: 'PK',
      pairingCode: 'CODE',
      socketFactory: factory,
      inFlight: () => ['d1'],
    });
    const p = t.connect();
    FakeSocket.last!.fireOpen();
    await p;

    expect(FakeSocket.last!.sentFrames()[0]).toMatchObject({
      type: 'hello',
      auth: 'TKN',
      agent_id: 'PK',
      pairing: 'CODE',
      resume: ['d1'],
    });
  });

  it('queues frames sent before open and flushes them after connect', async () => {
    const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, reconnect: false });
    const p = t.connect();
    await t.send({ v: 1, type: 'pong', ts: 1 }); // socket still CONNECTING -> queued
    FakeSocket.last!.fireOpen();
    await p;

    expect(FakeSocket.last!.sentFrames().map((f) => f.type)).toEqual(['hello', 'pong']);
  });

  it('parses inbound frames and invokes the handler', async () => {
    const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, reconnect: false });
    const got: InboundFrame[] = [];
    t.onFrame((f) => got.push(f));
    const p = t.connect();
    FakeSocket.last!.fireOpen();
    await p;

    FakeSocket.last!.fireMessage(JSON.stringify({ v: 1, type: 'ping', ts: 9 }));
    expect(got).toEqual([{ v: 1, type: 'ping', ts: 9 }]);
  });

  it('tolerates malformed inbound data without throwing or dispatching', async () => {
    const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, reconnect: false });
    const got: InboundFrame[] = [];
    t.onFrame((f) => got.push(f));
    const p = t.connect();
    FakeSocket.last!.fireOpen();
    await p;

    FakeSocket.last!.fireMessage('not json at all');
    FakeSocket.last!.fireMessage(JSON.stringify({ no: 'type field' }));
    expect(got).toEqual([]);
  });

  it('rejects connect when reconnect is disabled and the socket closes before open', async () => {
    const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, reconnect: false });
    const p = t.connect();
    FakeSocket.last!.close();
    await expect(p).rejects.toThrow();
  });

  it('reconnects with backoff after an unexpected close', async () => {
    vi.useFakeTimers();
    try {
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, backoffBaseMs: 100, random: () => 0 });
      const p = t.connect();
      const first = FakeSocket.last!;
      first.fireOpen();
      await p;

      first.close();
      await vi.advanceTimersByTimeAsync(99);
      expect(FakeSocket.last).toBe(first);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakeSocket.last).not.toBe(first); // a fresh socket was dialed
      await t.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('adds positive jitter without reducing the backoff floor', async () => {
    vi.useFakeTimers();
    try {
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, backoffBaseMs: 100, random: () => 1 });
      const p = t.connect();
      const first = FakeSocket.last!;
      first.fireOpen();
      await p;

      first.close(); // 100ms floor plus 20ms positive jitter
      await vi.advanceTimersByTimeAsync(119);
      expect(FakeSocket.last).toBe(first);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakeSocket.last).not.toBe(first);
      await t.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('uses at least 30 seconds after Retry-After 14 and caps fallback at five minutes', async () => {
    vi.useFakeTimers();
    try {
      const delays: number[] = [];
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, random: () => 1,
        log: (msg, extra) => { if (msg === 'mesh reconnect scheduled') delays.push(extra!.in_ms as number); } });
      void t.connect();
      const expected = [36_000, 72_000, 144_000, 288_000, 300_000, 300_000];
      for (const delay of expected) {
        const previous = FakeSocket.last!;
        previous.fireUpgradeRejected(429, '14');
        // Closing/error events from this socket must not schedule extra retries.
        previous.fireError({ message: 'secret server error' });
        previous.close();
        await vi.advanceTimersByTimeAsync(delay - 1);
        expect(FakeSocket.last).toBe(previous);
        await vi.advanceTimersByTimeAsync(1);
        expect(FakeSocket.last).not.toBe(previous);
      }
      expect(delays).toEqual(expected);
      await t.close();
    } finally { vi.useRealTimers(); }
  });

  it.each([
    ['delta seconds', '900', 900_000],
    ['HTTP date', 'Sun, 18 Oct 2026 12:15:00 GMT', 900_000],
    ['obsolete HTTP date', 'Tuesday, 20-Oct-26 12:15:00 GMT', 173_700_000],
    ['asctime HTTP date', 'Sun Oct 18 12:15:00 2026', 900_000],
  ])('honors %s cooldowns longer than the fallback cap', async (_kind, retryAfter, delayMs) => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-10-18T12:00:00Z'));
    try {
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, random: () => 0 });
      void t.connect();
      const first = FakeSocket.last!;
      first.fireUpgradeRejected(429, retryAfter);
      await vi.advanceTimersByTimeAsync(delayMs - 1);
      expect(FakeSocket.last).toBe(first);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakeSocket.last).not.toBe(first);
      await t.close();
    } finally { vi.useRealTimers(); }
  });

  it.each(['not-a-date-secret', '-1', '1.5', '0', 'Sun, 18 Oct 2026 11:59:59 GMT'])('falls back safely for invalid or elapsed Retry-After %s', async (retryAfter) => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-10-18T12:00:00Z'));
    try {
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, random: () => 0 });
      void t.connect();
      const first = FakeSocket.last!;
      first.fireUpgradeRejected(429, retryAfter);
      await vi.advanceTimersByTimeAsync(29_999);
      expect(FakeSocket.last).toBe(first);
      await vi.advanceTimersByTimeAsync(1);
      expect(FakeSocket.last).not.toBe(first);
      await t.close();
    } finally { vi.useRealTimers(); }
  });

  it('does not overflow long Retry-After timers or reconnect after intentional close', async () => {
    vi.useFakeTimers();
    try {
      const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, random: () => 0 });
      void t.connect();
      const first = FakeSocket.last!;
      first.fireUpgradeRejected(429, '2147485');
      await vi.advanceTimersByTimeAsync(2_147_483_647);
      expect(FakeSocket.last).toBe(first);
      expect(vi.getTimerCount()).toBe(1);
      await t.close();
      expect(vi.getTimerCount()).toBe(0);
      await vi.advanceTimersByTimeAsync(2_000);
      expect(FakeSocket.last).toBe(first);
    } finally { vi.useRealTimers(); }
  });

  it('handles a real rejected upgrade without sending hello or logging response secrets', async () => {
    const logs: unknown[] = [];
    const receivedFrames: Buffer[] = [];
    let requests = 0;
    const server = createServer();
    server.on('upgrade', (request, socket) => {
      requests++;
      expect(request.headers.authorization).toBeUndefined();
      socket.on('data', (data: Buffer) => receivedFrames.push(data));
      socket.end('HTTP/1.1 429 Too Many Requests\r\nRetry-After: 14\r\nX-Secret: response-header-secret\r\nContent-Length: 20\r\nConnection: close\r\n\r\nresponse-body-secret');
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('test listener unavailable');
    const t = new WsTransport({ url: `ws://127.0.0.1:${address.port}`, token: 'credential-secret', reconnect: false,
      log: (msg, extra) => logs.push({ msg, extra }) });
    try {
      await expect(t.connect()).rejects.toThrow('mesh upgrade rejected');
      expect(requests).toBe(1);
      expect(logs).toContainEqual({ msg: 'mesh upgrade rejected', extra: { status: 429, retry_after_ms: 14_000 } });
      expect(JSON.stringify(logs)).not.toMatch(/secret|hello/);
    } finally {
      await t.close();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    expect(receivedFrames).toEqual([]);
  });

  it('preserves the hello contract with the production ws client', async () => {
    const server = new WebSocketServer({ port: 0, host: '127.0.0.1' });
    await new Promise<void>((resolve) => server.once('listening', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('test listener unavailable');
    const hello = new Promise<unknown>((resolve) => server.once('connection', (socket) => socket.once('message', (data) => resolve(JSON.parse(String(data))))));
    const t = new WsTransport({ url: `ws://127.0.0.1:${address.port}`, token: 'test-token', agentId: 'agent', connectorId: 'connector', pairingCode: 'pair', inFlight: () => ['run-1'], reconnect: false });
    try {
      await t.connect();
      expect(await hello).toEqual({ v: 1, type: 'hello', auth: 'test-token', agent_id: 'agent', connector_id: 'connector', pairing: 'pair', resume: ['run-1'] });
    } finally {
      await t.close();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });

  async function openTransport(): Promise<{ got: InboundFrame[] }> {
    const t = new WsTransport({ url: 'wss://mesh', token: 'T', socketFactory: factory, reconnect: false });
    const got: InboundFrame[] = [];
    t.onFrame((f) => got.push(f));
    const p = t.connect();
    FakeSocket.last!.fireOpen();
    await p;
    return { got };
  }

  it('decodes a JSON frame delivered as a binary ArrayBuffer', async () => {
    const { got } = await openTransport();
    const bytes = new TextEncoder().encode(JSON.stringify({ v: 1, type: 'ping', ts: 7 }));
    FakeSocket.last!.fireMessage(bytes.buffer);
    expect(got).toEqual([{ v: 1, type: 'ping', ts: 7 }]);
  });

  it('decodes a JSON frame delivered as a binary Uint8Array view', async () => {
    const { got } = await openTransport();
    FakeSocket.last!.fireMessage(new TextEncoder().encode(JSON.stringify({ v: 1, type: 'pong', ts: 8 })));
    expect(got).toEqual([{ v: 1, type: 'pong', ts: 8 }]);
  });

  it('coerces a stringifiable inbound wrapper via toString', async () => {
    const { got } = await openTransport();
    FakeSocket.last!.fireMessage({ toString: () => JSON.stringify({ v: 1, type: 'ping', ts: 5 }) });
    expect(got).toEqual([{ v: 1, type: 'ping', ts: 5 }]);
  });

  it('reports socket state without claiming authentication or logging raw errors', async () => {
    const logs: Array<{ msg: string; extra?: Record<string, unknown> }> = [];
    const t = new WsTransport({
      url: 'wss://mesh',
      token: 'T',
      socketFactory: factory,
      reconnect: false,
      log: (msg, extra) => logs.push({ msg, extra }),
    });
    const p = t.connect();
    FakeSocket.last!.fireOpen();
    await p;
    expect(t.connectionState).toMatchObject({ socket: 'open', authentication: 'unverified' });

    FakeSocket.last!.fireError({ error: { code: 'ECONNREFUSED', message: 'boom' } });
    FakeSocket.last!.fireError('raw-error');
    const errs = logs.filter((l) => l.msg === 'mesh socket error');
    expect(errs).toEqual([{ msg: 'mesh socket error', extra: { code: 'ECONNREFUSED' } }]);
    expect(JSON.stringify(logs)).not.toMatch(/boom|raw-error/);
    expect(t.connectionState).toMatchObject({ socket: 'disconnected', authentication: 'unverified' });
  });
});
