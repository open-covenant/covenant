// Entry point. Boots a local health server (Render health check) and the connector,
// which DIALS OUT to the Hatcher mesh over a real WebSocket (WsTransport). The mesh
// connection runs in the background with backoff reconnect, so the service stays up
// even if the mesh is briefly unreachable.

import { readFileSync } from 'node:fs';
import Fastify from 'fastify';
import pino from 'pino';
import { loadConfig } from './config.js';
import { HttpDaemonClient } from './daemon.js';
import { Connector } from './connector.js';
import { WsTransport } from './wsTransport.js';
import { parseManifest, type ManifestCapability } from './manifest.js';
import { registerArtifactProxy } from './artifacts.js';

async function main(): Promise<void> {
  const log = pino({ level: process.env.LOG_LEVEL ?? 'info' });
  const cfg = loadConfig();

  if (!cfg.usingConnectorPeer) {
    log.warn(
      'using COVENANT_OPERATOR_TOKEN: per-dispatch grants will not constrain an over-privileged operator. Pair a scoped connector peer (COVENANT_CONNECTOR_TOKEN) for real least-privilege.',
    );
  }

  let manifestCapabilities: ManifestCapability[] = [];
  if (cfg.manifestPath) {
    try {
      const manifest = parseManifest(JSON.parse(readFileSync(cfg.manifestPath, 'utf8')));
      manifestCapabilities = manifest.capabilities;
      log.info({ agent: manifest.agent.name, capabilities: manifestCapabilities.length }, 'loaded manifest');
    } catch (err) {
      log.error({ err: String(err), path: cfg.manifestPath }, 'failed to load manifest; continuing with baseline grants only');
    }
  }

  const daemon = new HttpDaemonClient(cfg.daemonUrl, cfg.daemonToken, cfg.daemonTimeoutMs, cfg.daemonRetries);

  let connector: Connector;
  const transport = new WsTransport({
    url: cfg.hatcherUrl,
    token: cfg.hatcherToken,
    backoffBaseMs: cfg.hatcherReconnectBaseMs,
    backoffMaxMs: cfg.hatcherReconnectMaxMs,
    pairingCode: cfg.pairingCode,
    agentId: cfg.agentId,
    connectorId: process.env.HATCHER_CONNECTOR_ID,
    inFlight: () => connector?.inFlightIds ?? [],
    log: (msg, extra) => log.info(extra ?? {}, msg),
  });

  connector = new Connector(daemon, transport, {
    maxConcurrentDispatch: cfg.maxConcurrentDispatch,
    defaultDeadlineMs: cfg.defaultDeadlineMs,
    manifestCapabilities,
    agentId: cfg.agentId,
    gpuEnabled: cfg.gpuEnabled,
    log: (msg, extra) => log.info(extra ?? {}, msg),
  });

  const app = Fastify({ logger: false });
  app.get('/healthz', async () => {
    const daemonUp = await daemon.health();
    return { status: daemonUp && transport.connectionState.socket === 'open' ? 'ok' : 'degraded', daemon: daemonUp, mesh: transport.connectionState, gpu_enabled: cfg.gpuEnabled, in_flight: connector.inFlight };
  });
  if (cfg.artifactOrigin) registerArtifactProxy(app, cfg.artifactOrigin);
  await app.listen({ port: cfg.port, host: '0.0.0.0' });

  // Connect to the mesh in the background so boot doesn't block on mesh availability;
  // WsTransport retries with capped backoff.
  void connector.start().catch((err) => log.error({ err: String(err) }, 'mesh connect loop ended'));

  log.info({ port: cfg.port, gpu_enabled: cfg.gpuEnabled }, 'hatcher-connector up (transport: ws; authentication unverified)');
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error('fatal:', err);
  process.exit(1);
});
