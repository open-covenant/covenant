// Real outbound WebSocket transport to the Hatcher mesh, implementing
// HatcherTransport. The connector DIALS OUT — the covenant daemon never listens
// on a public interface (design §2.2). Speaks the proposed covenant.connector-mesh.v0
// frame contract (transport.ts). Auth is in-band via the `hello` frame: the
// bearer travels in the first frame over WSS instead of an Authorization header.
//
// Reconnect uses capped exponential backoff with jitter; on each (re)connect it
// re-announces `hello` carrying the agent id, pairing code, and the set of
// in-flight dispatch ids so the mesh can re-attach or cancel. The ws client exposes
// rejected upgrade responses so Retry-After can be honored without global hooks.

import WebSocket from 'ws';
import type { HatcherTransport, InboundFrame, OutboundFrame } from './transport.js';

interface UpgradeRejection {
  status: number;
  retryAfter?: string;
}

export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: 'open', listener: () => void): void;
  addEventListener(type: 'close', listener: () => void): void;
  addEventListener(type: 'error', listener: (ev: unknown) => void): void;
  addEventListener(type: 'message', listener: (ev: { data: unknown }) => void): void;
  onUpgradeRejected?(listener: (response: UpgradeRejection) => void): void;
}

export type SocketFactory = (url: string) => WebSocketLike;

const WS_OPEN = 1;
const MAX_TIMER_MS = 2_147_483_647;

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

function nodeFactory(url: string): WebSocketLike {
  const socket = new WebSocket(url, { handshakeTimeout: 15_000, followRedirects: false });
  return Object.assign(socket, {
    onUpgradeRejected(listener: (response: UpgradeRejection) => void): void {
      socket.on('unexpected-response', (_request, response) => {
        listener({ status: response.statusCode ?? 0, retryAfter: response.headers['retry-after'] });
        response.destroy();
      });
    },
  });
}

export class WsTransport implements HatcherTransport {
  private ws: WebSocketLike | null = null;
  private handler: ((f: InboundFrame) => void) | null = null;
  private readonly outbox: OutboundFrame[] = [];
  private closed = false;
  private attempt = 0;
  // Dedupes reconnect scheduling when a socket fires both `error` and `close`;
  // reset at the start of each dial so a failed redial can reschedule.
  private reconnecting = false;
  private reconnectTimer?: ReturnType<typeof setTimeout>;
  private firstOpen?: { resolve: () => void; reject: (e: unknown) => void };
  private socketState: 'disconnected' | 'connecting' | 'open' | 'reconnecting' | 'closed' = 'disconnected';

  get connectionState(): { socket: string; authentication: 'unverified'; reconnect_attempt: number } {
    // No authentication acknowledgement is defined in our pinned mesh contract.
    // Socket open/hello sent must never be advertised as authenticated or paired.
    return { socket: this.socketState, authentication: 'unverified', reconnect_attempt: this.attempt };
  }

  private readonly reconnect: boolean;
  private readonly backoffBaseMs: number;
  private readonly backoffMaxMs: number;
  private readonly log: (m: string, e?: Record<string, unknown>) => void;
  private readonly socketFactory: SocketFactory;
  private readonly random: () => number;

  constructor(private readonly opts: WsTransportOptions) {
    this.reconnect = opts.reconnect ?? true;
    this.backoffBaseMs = opts.backoffBaseMs ?? 30_000;
    this.backoffMaxMs = opts.backoffMaxMs ?? 300_000;
    if (!Number.isSafeInteger(this.backoffBaseMs) || this.backoffBaseMs <= 0
      || !Number.isSafeInteger(this.backoffMaxMs) || this.backoffMaxMs < this.backoffBaseMs) {
      throw new Error('invalid mesh reconnect backoff bounds');
    }
    this.log = opts.log ?? (() => {});
    this.socketFactory = opts.socketFactory ?? nodeFactory;
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
    this.socketState = 'closed';
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
    try {
      this.ws?.close();
    } finally {
      this.ws = null;
    }
  }

  private dial(): void {
    if (this.closed) return;
    this.socketState = 'connecting';
    this.reconnecting = false;
    let ws: WebSocketLike;
    try {
      ws = this.socketFactory(this.opts.url);
    } catch (err) {
      return this.scheduleReconnect(err);
    }
    this.ws = ws;
    ws.onUpgradeRejected?.((response) => {
      if (this.ws !== ws || this.closed) return;
      const retryAfterMs = parseRetryAfter(response.retryAfter);
      this.log('mesh upgrade rejected', {
        ...(Number.isInteger(response.status) && response.status >= 100 && response.status <= 599 ? { status: response.status } : {}),
        ...(retryAfterMs === undefined ? {} : { retry_after_ms: retryAfterMs }),
      });
      this.ws = null;
      this.socketState = 'disconnected';
      try { ws.close(); } catch { /* already disconnected */ }
      this.scheduleReconnect(new Error('mesh upgrade rejected'), retryAfterMs);
    });
    ws.addEventListener('open', () => {
      if (this.ws !== ws || this.closed) return;
      this.socketState = 'open';
      this.attempt = 0;
      this.announce();
      this.flush();
      this.firstOpen?.resolve();
      this.firstOpen = undefined;
      this.log('mesh socket open; hello sent (authentication unverified)');
    });
    ws.addEventListener('message', (ev) => { if (this.ws === ws && !this.closed) this.onMessage(ev.data); });
    // Some WebSocket impls fire `error` without a following `close`; reconnect on
    // either so the socket can never wedge half-open (scheduleReconnect dedupes).
    ws.addEventListener('error', (ev) => {
      if (this.ws !== ws || this.closed) return;
      const code = safeErrorCode(ev);
      this.log('mesh socket error', code === undefined ? undefined : { code });
      this.ws = null;
      this.socketState = 'disconnected';
      try { ws.close(); } catch { /* already disconnected */ }
      this.scheduleReconnect(new Error('socket error'));
    });
    ws.addEventListener('close', () => {
      if (this.ws !== ws || this.closed) return;
      this.ws = null;
      this.socketState = 'disconnected';
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
      return this.log('mesh sent non-JSON frame');
    }
    if (!frame || typeof (frame as { type?: unknown }).type !== 'string') return;
    this.handler?.(frame as InboundFrame);
  }

  private rawSend(frame: OutboundFrame): void {
    try {
      this.ws?.send(JSON.stringify(frame));
    } catch (err) {
      this.outbox.push(frame);
      this.log('mesh send failed, queued');
    }
  }

  private flush(): void {
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    for (const f of this.outbox.splice(0)) this.rawSend(f);
  }

  private scheduleReconnect(err: unknown, retryAfterMs = 0): void {
    if (this.firstOpen && !this.reconnect) {
      this.firstOpen.reject(err);
      this.firstOpen = undefined;
    }
    if (this.closed || !this.reconnect) return;
    if (this.reconnecting) return; // error + close both fire: schedule once per socket
    this.reconnecting = true;
    this.socketState = 'reconnecting';
    const exp = Math.min(this.backoffMaxMs, this.backoffBaseMs * 2 ** Math.min(this.attempt++, 30));
    const random = Math.min(1, Math.max(0, this.random())) || 0;
    const jitter = Math.round(exp * 0.2 * random);
    const fallback = Math.min(this.backoffMaxMs, exp + jitter);
    // Jitter only adds delay. A server cooldown can exceed the fallback cap.
    const delay = Math.max(fallback, Math.min(Number.MAX_SAFE_INTEGER, retryAfterMs + jitter));
    this.log('mesh reconnect scheduled', { in_ms: delay, attempt: this.attempt });
    this.scheduleDial(delay);
  }

  private scheduleDial(delayMs: number): void {
    // Node overflows timers above this limit to 1 ms. Split long cooldowns so a
    // valid, distant Retry-After can never accidentally trigger an immediate retry.
    const chunk = Math.min(delayMs, MAX_TIMER_MS);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined;
      if (this.closed) return;
      if (delayMs > chunk) this.scheduleDial(delayMs - chunk);
      else this.dial();
    }, chunk);
  }
}

function parseRetryAfter(value: string | undefined, now = Date.now()): number | undefined {
  if (value === undefined) return undefined;
  const trimmed = value.trim();
  if (/^\d+$/.test(trimmed)) return Math.min(Number.MAX_SAFE_INTEGER, Number(trimmed) * 1_000);
  // Date.parse also accepts bare numbers and loose dates that are not HTTP dates.
  if (!/^(?:(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)(?:,| )|(?:Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday),)/.test(trimmed)) return undefined;
  // The obsolete asctime form has no zone token but still means GMT in HTTP.
  const date = Date.parse(/^[A-Z][a-z]{2} [A-Z][a-z]{2} /.test(trimmed) ? `${trimmed} GMT` : trimmed);
  return Number.isFinite(date) ? Math.max(0, date - now) : undefined;
}

function safeErrorCode(event: unknown): string | undefined {
  if (!event || typeof event !== 'object') return undefined;
  const error = 'error' in event ? event.error : event;
  if (!error || typeof error !== 'object' || !('code' in error)) return undefined;
  return typeof error.code === 'string' && /^(?:E[A-Z_]{2,50}|UND_ERR_[A-Z_]+)$/.test(error.code) ? error.code : undefined;
}

function toText(data: unknown): string | null {
  if (typeof data === 'string') return data;
  if (data instanceof ArrayBuffer) return new TextDecoder().decode(data);
  if (ArrayBuffer.isView(data)) return new TextDecoder().decode(data as Uint8Array);
  if (data && typeof (data as { toString?: unknown }).toString === 'function') return String(data);
  return null;
}
