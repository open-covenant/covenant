import { describe, it, expect, vi } from 'vitest';
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

      first.close(); // unexpected -> schedule a reconnect (exp=100, delay=50 with random()=0)
      await vi.advanceTimersByTimeAsync(60);
      expect(FakeSocket.last).not.toBe(first); // a fresh socket was dialed
    } finally {
      vi.useRealTimers();
    }
  });
});
