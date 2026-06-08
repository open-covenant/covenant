// Hatcher mesh transport — the covenant.connector-mesh.v1 frame contract, confirmed
// compatible by the Hatcher team (2026-06-04). The connector logic (connector.ts) is
// written against this interface and stays testable with the in-memory StubTransport.

import type { AgentEvent } from './daemon.js';

// A per-dispatch grant from Hatcher's consent UI. The connector maps each scope to a
// covenant capability grant (token-gated: github/mcp/a2a) or a consent-policy entry
// (sandbox-gated + audited: filesystem/terminal/browser). See capabilities.mapMeshGrants.
export interface MeshGrant {
  scope: string;
  constraints?: Record<string, unknown>;
}

// Hatcher -> connector frames.
export type InboundFrame =
  | {
      v: 1;
      type: 'dispatch';
      dispatch_id: string;
      agent_id: string;
      intent: { text: string; context?: Record<string, unknown> };
      grants?: MeshGrant[];
      deadline_ms?: number;
      reply_to?: string;
    }
  | { v: 1; type: 'cancel'; dispatch_id: string }
  | { v: 1; type: 'ping'; ts: number };

// connector -> Hatcher frames.
export type OutboundFrame =
  // Sent first on every (re)connect: in-band auth + resume of in-flight dispatch ids
  // (WHATWG WebSocket cannot set an Authorization header, so the bearer travels in-band
  // over WSS). connector_id is the stable per-daemon identity, distinct from agent_id —
  // one local daemon may serve several Hatcher agents.
  | { v: 1; type: 'hello'; connector_id?: string; agent_id?: string; auth?: string; pairing?: string; resume?: string[] }
  | { v: 1; type: 'accepted'; dispatch_id: string; intent_id: string }
  | { v: 1; type: 'trace'; dispatch_id: string; seq: number; event: AgentEvent }
  | { v: 1; type: 'result'; dispatch_id: string; status: string; result: unknown; proof: unknown }
  | { v: 1; type: 'error'; dispatch_id: string; code: string; message: string }
  | { v: 1; type: 'pong'; ts: number };

export interface HatcherTransport {
  // Resolves once the outbound channel is established + paired.
  connect(): Promise<void>;
  // Register the handler invoked for each inbound frame.
  onFrame(handler: (frame: InboundFrame) => void): void;
  // Push a frame up to Hatcher.
  send(frame: OutboundFrame): Promise<void>;
  close(): Promise<void>;
}

// In-memory stub used by tests and local dry-runs. Lets you drive dispatch
// frames into the connector and capture what it sends back, with zero network.
export class StubTransport implements HatcherTransport {
  private handler: ((frame: InboundFrame) => void) | null = null;
  readonly sent: OutboundFrame[] = [];

  async connect(): Promise<void> {}

  onFrame(handler: (frame: InboundFrame) => void): void {
    this.handler = handler;
  }

  async send(frame: OutboundFrame): Promise<void> {
    this.sent.push(frame);
  }

  async close(): Promise<void> {}

  // Test/dry-run driver: simulate Hatcher pushing a frame down.
  inject(frame: InboundFrame): void {
    if (!this.handler) throw new Error('no frame handler registered');
    this.handler(frame);
  }
}
