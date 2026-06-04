import { describe, it, expect, afterEach } from 'vitest';
import { MeshServer } from './meshServer.js';
import { WsTransport } from '../wsTransport.js';
import { Connector } from '../connector.js';
import { ScriptedDaemon, DEMO_CAPS, DEMO_TRACE } from './scenario.js';

describe('MeshServer × WsTransport × Connector over real loopback sockets', () => {
  let mesh: MeshServer | undefined;
  let transport: WsTransport | undefined;

  afterEach(async () => {
    await transport?.close();
    await mesh?.stop();
    mesh = undefined;
    transport = undefined;
  });

  it('round-trips a dispatch to a proof envelope over a real WebSocket', async () => {
    mesh = new MeshServer({ expectedAuth: 'demo-auth' });
    const port = await mesh.start();

    const daemon = new ScriptedDaemon(DEMO_TRACE);
    transport = new WsTransport({ url: `ws://127.0.0.1:${port}`, token: 'demo-auth', agentId: 'A', reconnect: false });
    const connector = new Connector(daemon, transport, {
      maxConcurrentDispatch: 2,
      defaultDeadlineMs: 60_000,
      manifestCapabilities: DEMO_CAPS,
    });
    await connector.start();
    await mesh.whenConnected();

    const final = await mesh.dispatch({
      v: 1,
      type: 'dispatch',
      dispatch_id: 'd1',
      agent_id: 'A',
      intent: { text: 'run the tests' },
      deadline_ms: Date.now() + 60_000,
    });

    expect(final.type).toBe('result');
    if (final.type === 'result') {
      expect(final.status).toBe('ok');
      const proof = final.proof as { audit_root: string; events: unknown[] };
      expect(proof.audit_root).toBe('a1b2c3d4e5f60718');
      expect(proof.events).toHaveLength(4);
    }
  });

  it('rejects a connector that presents the wrong in-band auth token', async () => {
    mesh = new MeshServer({ expectedAuth: 'right-token' });
    const port = await mesh.start();

    transport = new WsTransport({ url: `ws://127.0.0.1:${port}`, token: 'wrong-token', reconnect: false });
    const connector = new Connector(new ScriptedDaemon([]), transport, {
      maxConcurrentDispatch: 1,
      defaultDeadlineMs: 1000,
      manifestCapabilities: [],
    });
    await connector.start();

    await expect(mesh.whenConnected(400)).rejects.toThrow(/no connector connected/);
  });
});
