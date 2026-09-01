import { assertBootConfig, assertLiveConfig, loadConfig } from './config.js';
import { UsePodContributorReviewer } from './contributor-reviewer.js';
import { JobProcessor } from './executor.js';
import { GithubClient } from './github.js';
import { PolicySignerClient } from './policy-client.js';
import { runPredeploy } from './predeploy.js';
import { createServiceReadiness } from './service-readiness.js';
import { PostgresStore, type MizukiStore } from './store.js';
import { UpdaterStatusClient } from './updater-client.js';
import { Payments } from './x402.js';

const config = loadConfig();
assertBootConfig(config);

await runPredeploy({
  connect: async () => {
    if (!config.databaseUrl) throw new Error('MIZUKI_DATABASE_URL is required for predeploy');
    return PostgresStore.connect(config.databaseUrl);
  },
  assertStaticConfig: () => assertLiveConfig(config),
  checkReadiness: (store) => readiness(store).check(),
});

function readiness(store: MizukiStore) {
  const github = new GithubClient(config);
  const payments = new Payments(config);
  const policy = new PolicySignerClient(config);
  const reviewer = new UsePodContributorReviewer(config, store, github);
  const processor = new JobProcessor(config, store, github, fetch, async () => {}, policy);
  const updater =
    config.updaterUrl && config.updaterToken
      ? new UpdaterStatusClient(config.updaterUrl, config.updaterToken, config.updaterTimeoutMs)
      : undefined;
  return createServiceReadiness({
    config,
    store,
    processor,
    policy,
    github,
    reviewer,
    updater,
    payments,
  });
}
