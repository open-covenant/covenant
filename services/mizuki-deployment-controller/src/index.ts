import { HttpArtifactGateway } from './artifact.js';
import { loadConfig } from './config.js';
import { DeploymentController } from './controller.js';
import { HttpApplicationGateway } from './probe.js';
import { RenderClient } from './render.js';
import { createControllerServer } from './server.js';
import { PostgresOperationStore } from './store.js';

const config = loadConfig();
const store = new PostgresOperationStore(config.database);
const render = new RenderClient({
  apiUrl: config.renderApiUrl,
  apiKey: config.renderApiKey,
  allowedServiceIds: config.allowedServiceIds,
  timeoutMs: config.renderTimeoutMs,
});
const artifacts = new HttpArtifactGateway(config.artifactOrigins, config.artifactTimeoutMs);
const applications = new HttpApplicationGateway({
  targets: new Map([
    [config.shadowServiceId, { role: 'shadow', url: config.shadowProbeUrl } as const],
    [
      config.productionServiceId,
      {
        role: 'production',
        url: config.productionProbeUrl,
        token: config.productionProbeToken,
      } as const,
    ],
  ]),
  timeoutMs: config.probeTimeoutMs,
});
const controller = new DeploymentController(config, store, render, artifacts, applications);

await store.migrate();
const server = createControllerServer({ controller, store, authToken: config.authToken });
server.headersTimeout = 10_000;
server.requestTimeout = 120_000;
server.keepAliveTimeout = 5_000;
server.listen(config.port, config.host, () => {
  process.stdout.write(`mizuki deployment controller listening on ${config.host}:${config.port}\n`);
});

let stopping = false;
async function shutdown(): Promise<void> {
  if (stopping) return;
  stopping = true;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await store.close();
}

process.once('SIGINT', () => void shutdown().finally(() => process.exit(0)));
process.once('SIGTERM', () => void shutdown().finally(() => process.exit(0)));
