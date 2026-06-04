// Standalone reference Hatcher mesh PROCESS: a real ws server (covenant.connector-mesh.v0)
// plus a small HTTP control plane so a human can drive dispatches by hand:
//   GET  /status            -> { mesh_port, connector_online }
//   POST /dispatch {text}    -> { dispatch_id, status, proof }  (the connector-trace proof)
//
// Run it next to the connector (which dials this server) and the local daemon. Every
// frame is logged so you can watch the protocol. Env: MESH_PORT (ws), MESH_CONTROL_PORT
// (http), MESH_AUTH (the in-band token the connector must present in `hello`).

import Fastify from 'fastify';
import { MeshServer } from './meshServer.js';

const MESH_PORT = Number(process.env.MESH_PORT ?? 8788);
const CONTROL_PORT = Number(process.env.MESH_CONTROL_PORT ?? 8789);
const AUTH = process.env.MESH_AUTH ?? 'hatcher-connector-demo-auth-token';

const log = (m: string, e?: Record<string, unknown>) => console.log(`[mesh] ${m}${e ? ' ' + JSON.stringify(e) : ''}`);

async function main(): Promise<void> {
  const mesh = new MeshServer({ port: MESH_PORT, expectedAuth: AUTH, log });
  const port = await mesh.start();
  log(`reference Hatcher mesh listening on ws://127.0.0.1:${port} (connector must present auth: ${AUTH})`);

  const app = Fastify({ logger: false });

  app.get('/status', async () => ({ mesh_port: port, connector_online: mesh.connectorOnline }));

  app.post('/dispatch', async (req, reply) => {
    const body = (req.body ?? {}) as { text?: string; deadline_ms?: number };
    const text = (body.text ?? '').trim();
    if (!text) return reply.code(400).send({ error: 'text required' });
    if (!mesh.connectorOnline) return reply.code(409).send({ error: 'no connector connected' });

    const dispatch_id = crypto.randomUUID();
    log(`→ dispatch ${dispatch_id}: ${text}`);
    try {
      const final = await mesh.dispatch(
        {
          v: 1,
          type: 'dispatch',
          dispatch_id,
          agent_id: 'control',
          intent: { text },
          deadline_ms: body.deadline_ms ?? Date.now() + 600_000,
        },
        90_000,
      );
      if (final.type === 'result') return { dispatch_id, status: final.status, proof: final.proof };
      return reply.code(502).send({ dispatch_id, error: final.code, message: final.message });
    } catch (err) {
      return reply.code(504).send({ dispatch_id, error: 'timeout', message: String(err) });
    }
  });

  await app.listen({ port: CONTROL_PORT, host: '127.0.0.1' });
  log(`control API on http://127.0.0.1:${CONTROL_PORT}  (GET /status · POST /dispatch {"text":"…"})`);
}

main().catch((err) => {
  console.error('mesh fatal:', err);
  process.exit(1);
});
