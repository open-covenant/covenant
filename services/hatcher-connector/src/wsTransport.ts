// Real outbound WebSocket transport to the Hatcher mesh, implementing
// HatcherTransport. The connector DIALS OUT — the covenant daemon never listens
// on a public interface (design §2.2). Speaks the proposed covenant.connector-mesh.v0
// frame contract (transport.ts). Auth is in-band via the `hello` frame: the
// WHATWG/Node global WebSocket cannot set request headers, so the bearer travels
// in the first frame over WSS instead of an Authorization header.
//
// Reconnect uses capped exponential backoff with jitter; on each (re)connect it
// re-announces `hello` carrying the agent id, pairing code, and the set of
// in-flight dispatch ids so the mesh can re-attach or cancel. Uses the Node
// global WebSocket by default — no runtime dependency — and accepts an injected
// socket factory for tests.

import type { HatcherTransport, InboundFrame, OutboundFrame } from './transport.js';

export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: 'open', listener: () => void): void;
  addEventListener(type: 'close', listener: () => void): void;
  addEventListener(type: 'error', listener: (ev: unknown) => void): void;
  addEventListener(type: 'message', listener: (ev: { data: unknown }) => void): void;
}

export type SocketFactory = (url: string) => WebSocketLike;

const WS_OPEN = 1;

export interface WsTransportOptions {
  url: string;
  token: string;
  pairingCode?: string;
  agentId?: string;
  connectorId?: string;
  reconnect?: boolean;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  // dispatch ids still in flight, announced on resume so the mesh can re-attach.
  inFlight?: () => string[];
  log?: (msg: string, extra?: Record<string, unknown>) => void;
  socketFactory?: SocketFactory;
  random?: () => number;
}

function nativeFactory(url: string): WebSocketLike {
  const Native = (globalThis as { WebSocket?: new (u: string) => WebSocketLike }).WebSocket;
  if (!Native) throw new Error('global WebSocket unavailable (need Node >= 22); inject socketFactory');
  return new Native(url);
}

export class WsTransport implements HatcherTransport {
  private ws: WebSocketLike | null = null;
  private handler: ((f: InboundFrame) => void) | null = null;
  private readonly outbox: OutboundFrame[] = [];
  private closed = false;
  private attempt = 0;
  private firstOpen?: { resolve: () => void; reject: (e: unknown) => void };

  private readonly reconnect: boolean;
  private readonly backoffBaseMs: number;
  private readonly backoffMaxMs: number;
  private readonly log: (m: string, e?: Record<string, unknown>) => void;
  private readonly socketFactory: SocketFactory;
  private readonly random: () => number;

  constructor(private readonly opts: WsTransportOptions) {
    this.reconnect = opts.reconnect ?? true;
    this.backoffBaseMs = opts.backoffBaseMs ?? 500;
    this.backoffMaxMs = opts.backoffMaxMs ?? 30_000;
    this.log = opts.log ?? (() => {});
    this.socketFactory = opts.socketFactory ?? nativeFactory;
    this.random = opts.random ?? Math.random;
  }

  connect(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.firstOpen = { resolve, reject };
      this.dial();
    });
  }

  onFrame(handler: (f: InboundFrame) => void): void {
    this.handler = handler;
  }

  send(frame: OutboundFrame): Promise<void> {
    if (this.ws && this.ws.readyState === WS_OPEN) this.rawSend(frame);
    else this.outbox.push(frame);
    return Promise.resolve();
  }

  async close(): Promise<void> {
    this.closed = true;
    try {
      this.ws?.close();
    } finally {
      this.ws = null;
    }
  }

  private dial(): void {
    if (this.closed) return;
    let ws: WebSocketLike;
    try {
      ws = this.socketFactory(this.opts.url);
    } catch (err) {
      return this.scheduleReconnect(err);
    }
    this.ws = ws;
    ws.addEventListener('open', () => {
      this.attempt = 0;
      this.announce();
      this.flush();
      this.firstOpen?.resolve();
      this.firstOpen = undefined;
      this.log('mesh connected', { url: this.opts.url });
    });
    ws.addEventListener('message', (ev) => this.onMessage(ev.data));
    ws.addEventListener('error', (ev) => this.log('mesh socket error', { err: errText(ev) }));
    ws.addEventListener('close', () => {
      this.ws = null;
      this.scheduleReconnect(new Error('socket closed'));
    });
  }

  private announce(): void {
    this.rawSend({
      v: 1,
      type: 'hello',
      connector_id: this.opts.connectorId,
      agent_id: this.opts.agentId,
      auth: this.opts.token,
      pairing: this.opts.pairingCode,
      resume: this.opts.inFlight?.() ?? [],
    });
  }

  private onMessage(data: unknown): void {
    const text = toText(data);
    if (text === null) return;
    let frame: unknown;
    try {
      frame = JSON.parse(text);
    } catch {
      return this.log('mesh sent non-JSON frame', { sample: text.slice(0, 120) });
    }
    if (!frame || typeof (frame as { type?: unknown }).type !== 'string') return;
    this.handler?.(frame as InboundFrame);
  }

  private rawSend(frame: OutboundFrame): void {
    try {
      this.ws?.send(JSON.stringify(frame));
    } catch (err) {
      this.outbox.push(frame);
      this.log('mesh send failed, queued', { err: errText(err) });
    }
  }

  private flush(): void {
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    for (const f of this.outbox.splice(0)) this.rawSend(f);
  }

  private scheduleReconnect(err: unknown): void {
    if (this.firstOpen && !this.reconnect) {
      this.firstOpen.reject(err);
      this.firstOpen = undefined;
    }
    if (this.closed || !this.reconnect) return;
    const exp = Math.min(this.backoffMaxMs, this.backoffBaseMs * 2 ** this.attempt++);
    const delay = exp / 2 + this.random() * (exp / 2); // half-fixed + half-jitter
    this.log('mesh reconnect scheduled', { in_ms: Math.round(delay), attempt: this.attempt });
    setTimeout(() => this.dial(), delay);
  }
}

function toText(data: unknown): string | null {
  if (typeof data === 'string') return data;
  if (data instanceof ArrayBuffer) return new TextDecoder().decode(data);
  if (ArrayBuffer.isView(data)) return new TextDecoder().decode(data as Uint8Array);
  if (data && typeof (data as { toString?: unknown }).toString === 'function') return String(data);
  return null;
}

function errText(err: unknown): string {
  if (err && typeof err === 'object' && 'message' in err) return String((err as { message: unknown }).message);
  return String(err);
}
