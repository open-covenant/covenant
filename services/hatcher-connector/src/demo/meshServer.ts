// A real WebSocket mesh server — the REFERENCE implementation of the Hatcher side
// of the covenant.connector-mesh.v0 frame contract (transport.ts). Not a mock: real
// sockets, real frames. It stands in for Hatcher's hosted mesh (whose exact contract
// is still to be confirmed — handoff §7), and doubles as runnable reference code.
//
// Node's global WebSocket is client-only, so the server side uses the `ws` package.

import { WebSocketServer, type WebSocket, type RawData } from 'ws';
import type { AddressInfo } from 'node:net';
import type { InboundFrame, OutboundFrame } from '../transport.js';

type TerminalFrame = Extract<OutboundFrame, { type: 'result' | 'error' }>;

export interface MeshServerOptions {
  port?: number;
  host?: string;
  // If set, the connector's first `hello` frame must carry this in-band auth token.
  expectedAuth?: string;
  log?: (msg: string, extra?: Record<string, unknown>) => void;
}

export class MeshServer {
  private wss?: WebSocketServer;
  private conn?: WebSocket;
  private connected = false;
  private readonly connectedWaiters: Array<() => void> = [];
  private readonly waiters = new Map<string, (f: TerminalFrame) => void>();
  private readonly log: (m: string, e?: Record<string, unknown>) => void;
  port = 0;

  constructor(private readonly opts: MeshServerOptions = {}) {
    this.log = opts.log ?? (() => {});
  }

  get connectorOnline(): boolean {
    return this.connected && !!this.conn;
  }

  start(): Promise<number> {
    return new Promise((resolve, reject) => {
      const wss = new WebSocketServer({ host: this.opts.host ?? '127.0.0.1', port: this.opts.port ?? 0 });
      this.wss = wss;
      wss.once('error', reject);
      wss.once('listening', () => {
        this.port = (wss.address() as AddressInfo).port;
        this.log('mesh listening', { port: this.port });
        resolve(this.port);
      });
      wss.on('connection', (ws) => this.onConnection(ws));
    });
  }

  private onConnection(ws: WebSocket): void {
    this.conn = ws;
    ws.on('message', (data: RawData) => {
      let frame: OutboundFrame;
      try {
        frame = JSON.parse(data.toString()) as OutboundFrame;
      } catch {
        return;
      }
      this.onFrame(ws, frame);
    });
    ws.on('close', () => {
      if (this.conn === ws) {
        this.conn = undefined;
        this.connected = false;
      }
    });
  }

  private onFrame(ws: WebSocket, f: OutboundFrame): void {
    switch (f.type) {
      case 'hello':
        if (this.opts.expectedAuth && f.auth !== this.opts.expectedAuth) {
          this.log('connector rejected: bad auth');
          ws.close(4001, 'unauthorized');
          return;
        }
        this.log('connector paired', { agent_id: f.agent_id, resume: f.resume });
        this.connected = true;
        for (const w of this.connectedWaiters.splice(0)) w();
        break;
      case 'accepted':
        this.log('accepted', { dispatch_id: f.dispatch_id, intent_id: f.intent_id });
        break;
      case 'trace':
        this.log('trace', { dispatch_id: f.dispatch_id, seq: f.seq, event: f.event.type });
        break;
      case 'result':
        this.log('result', { dispatch_id: f.dispatch_id, status: f.status });
        this.waiters.get(f.dispatch_id)?.(f);
        this.waiters.delete(f.dispatch_id);
        break;
      case 'error':
        this.log('error', { dispatch_id: f.dispatch_id, code: f.code });
        this.waiters.get(f.dispatch_id)?.(f);
        this.waiters.delete(f.dispatch_id);
        break;
      case 'pong':
        break;
    }
  }

  // Resolves once a connector has connected AND passed the hello auth check.
  whenConnected(timeoutMs = 5000): Promise<void> {
    if (this.connected) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const onConn = () => {
        clearTimeout(timer);
        resolve();
      };
      const timer = setTimeout(() => {
        const i = this.connectedWaiters.indexOf(onConn);
        if (i >= 0) this.connectedWaiters.splice(i, 1);
        reject(new Error('no connector connected within timeout'));
      }, timeoutMs);
      this.connectedWaiters.push(onConn);
    });
  }

  // Send a dispatch DOWN to the connector; resolves with its terminal result/error,
  // or rejects if the connector doesn't answer within timeoutMs.
  dispatch(frame: Extract<InboundFrame, { type: 'dispatch' }>, timeoutMs = 120_000): Promise<TerminalFrame> {
    const conn = this.conn;
    if (!conn) return Promise.reject(new Error('no connector connected'));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiters.delete(frame.dispatch_id);
        reject(new Error(`dispatch ${frame.dispatch_id} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.waiters.set(frame.dispatch_id, (f) => {
        clearTimeout(timer);
        resolve(f);
      });
      conn.send(JSON.stringify(frame));
    });
  }

  async stop(): Promise<void> {
    this.conn?.close();
    await new Promise<void>((resolve) => {
      if (!this.wss) return resolve();
      this.wss.close(() => resolve());
    });
  }
}
