// Cross-process end-to-end demo over a REAL WebSocket: a real mesh server dispatches
// a developer task DOWN to the connector (real WsTransport, real socket), which runs
// it as a local covenant intent and round-trips the audited proof back UP.
//
//   OFFLINE (default): scripted daemon — deterministic, no covenantd needed.
//   LIVE: set COVENANT_DAEMON_URL + COVENANT_CONNECTOR_TOKEN (or COVENANT_OPERATOR_TOKEN)
//         to drive a real covenantd. Real WS mesh ↔ connector ↔ real daemon.

import { MeshServer } from './meshServer.js';
import { WsTransport } from '../wsTransport.js';
import { Connector } from '../connector.js';
import { HttpDaemonClient, type DaemonClient } from '../daemon.js';
import { ScriptedDaemon, DEMO_CAPS, DEMO_TRACE } from './scenario.js';

const AUTH = 'hatcher-connector-demo-auth-token';

function makeDaemon(): { daemon: DaemonClient; live: boolean } {
  const url = process.env.COVENANT_DAEMON_URL;
  const token = process.env.COVENANT_CONNECTOR_TOKEN ?? process.env.COVENANT_OPERATOR_TOKEN;
  if (url && token) return { daemon: new HttpDaemonClient(url.replace(/\/+$/, ''), token), live: true };
  return { daemon: new ScriptedDaemon(DEMO_TRACE), live: false };
}

const tag = (t: string) => (m: string, e?: Record<string, unknown>) =>
  console.log(`[${t}] ${m}${e ? ' ' + JSON.stringify(e) : ''}`);

async function main(): Promise<void> {
  const mesh = new MeshServer({ expectedAuth: AUTH, log: tag('mesh') });
  const port = await mesh.start();

  const { daemon, live } = makeDaemon();
  const transport = new WsTransport({
    url: `ws://127.0.0.1:${port}`,
    token: AUTH,
    agentId: 'covenant-local-1',
    reconnect: false,
    log: tag('ws'),
  });
  const connector = new Connector(daemon, transport, {
    maxConcurrentDispatch: 2,
    defaultDeadlineMs: 600_000,
    manifestCapabilities: DEMO_CAPS,
    log: tag('connector'),
  });

  await connector.start(); // real WS dial + in-band hello
  await mesh.whenConnected();

  const text =
    process.env.DEMO_INTENT ?? 'Inspect this repo, run the test suite, and write REPORT.md summarizing failures.';
  console.log(`\n── REAL WS mesh demo · daemon: ${live ? 'LIVE covenantd' : 'scripted'} ──`);
  console.log(`── path: mesh ws://127.0.0.1:${port} → connector → daemon`);
  console.log(`── dispatch: ${text}\n`);

  const final = await mesh.dispatch({
    v: 1,
    type: 'dispatch',
    dispatch_id: 'mesh-demo-1',
    agent_id: 'covenant-local-1',
    intent: { text },
    deadline_ms: Date.now() + 600_000,
  });

  if (final.type === 'result') {
    console.log('\n── proof envelope (round-tripped over a real WebSocket) ──');
    console.log(JSON.stringify(final.proof, null, 2));
    console.log(`\n✓ result "${final.status}" returned over real WS\n`);
  } else {
    console.log(`\n✗ ${final.code}: ${final.message}\n`);
  }

  await transport.close();
  await mesh.stop();
  process.exit(final.type === 'result' && final.status === 'success' ? 0 : 1);
}

main().catch((err) => {
  console.error('mesh-demo fatal:', err);
  process.exit(1);
});
