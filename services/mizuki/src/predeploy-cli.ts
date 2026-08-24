import { assertBootConfig, loadConfig } from './config.js';
import { runPredeploy } from './predeploy.js';
import { PostgresStore } from './store.js';

const config = loadConfig();
assertBootConfig(config);

await runPredeploy({
  connect: async () => {
    if (!config.databaseUrl) throw new Error('MIZUKI_DATABASE_URL is required for predeploy');
    return PostgresStore.connect(config.databaseUrl);
  },
});
