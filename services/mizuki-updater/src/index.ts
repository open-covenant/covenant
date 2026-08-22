import { loadConfig } from './config.js';
import { HttpDeploymentGateway } from './deployment.js';
import { GitHubAppGateway } from './github.js';
import { UpdaterMetrics } from './metrics.js';
import { PostgresUpgradeRepository } from './postgres.js';
import { createUpdaterServer } from './server.js';
import { InMemoryUpgradeRepository } from './store.js';
import { UpdaterService } from './updater.js';
import { HttpArtifactVerifier, ProposalVerifier } from './verification.js';

const config = loadConfig();
const repository = config.memoryStore
  ? new InMemoryUpgradeRepository()
  : new PostgresUpgradeRepository(config.databaseUrl!);
const metrics = new UpdaterMetrics();
const proposals = new ProposalVerifier({
  trustedProposalKeys: config.trustedProposalKeys,
  trustedBenchmarkKeys: config.trustedBenchmarkKeys,
  trustedReviewKeys: config.trustedReviewKeys,
  allowedRepositories: config.allowedRepositories,
  allowedBaseBranches: config.allowedBaseBranches,
  headBranchPrefix: config.headBranchPrefix,
  mandatoryChecks: config.mandatoryChecks,
  maxProposalAgeMs: config.proposalMaxAgeMs,
});
const artifacts = new HttpArtifactVerifier(
  config.artifactOrigins,
  config.artifactTimeoutMs,
  config.artifactMaxBytes,
);
const github = new GitHubAppGateway({
  apiUrl: config.githubApiUrl,
  appId: config.githubAppId,
  privateKey: config.githubPrivateKey,
  timeoutMs: config.githubTimeoutMs,
  mergeMethod: config.githubMergeMethod,
});
const deployments = new HttpDeploymentGateway({
  shadowUrl: config.shadowHookUrl,
  shadowHealthUrlTemplate: config.shadowHealthUrlTemplate,
  promotionHealthUrlTemplate: config.promotionHealthUrlTemplate,
  promoteUrl: config.promoteHookUrl,
  rollbackUrl: config.rollbackHookUrl,
  token: config.deployHookToken,
  timeoutMs: config.hookTimeoutMs,
});
const service = new UpdaterService(
  {
    checkTimeoutMs: config.checkTimeoutMs,
    healthTimeoutMs: config.healthTimeoutMs,
    promotionSoakMs: config.promotionSoakMs,
    promotionTimeoutMs: config.promotionTimeoutMs,
    pollIntervalMs: config.pollIntervalMs,
    leaseMs: config.leaseMs,
    maxAttempts: config.maxAttempts,
  },
  repository,
  proposals,
  artifacts,
  github,
  deployments,
  metrics,
);

await repository.migrate();
const server = createUpdaterServer({
  service,
  repository,
  metrics,
  authToken: config.authToken,
  readToken: config.readToken,
});
server.headersTimeout = 10_000;
server.requestTimeout = 15_000;
server.keepAliveTimeout = 5_000;
server.listen(config.port, config.host, () => {
  process.stdout.write(`mizuki updater listening on ${config.host}:${config.port}\n`);
});

let recovering = false;
async function recover(): Promise<void> {
  if (recovering) return;
  recovering = true;
  try {
    await service.recover();
  } catch {
    metrics.increment('errors');
  } finally {
    recovering = false;
  }
}

void recover();
const recovery = setInterval(() => void recover(), config.pollIntervalMs);
recovery.unref();

let stopping = false;
async function shutdown(): Promise<void> {
  if (stopping) return;
  stopping = true;
  clearInterval(recovery);
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await repository.close();
}

process.once('SIGINT', () => void shutdown().finally(() => process.exit(0)));
process.once('SIGTERM', () => void shutdown().finally(() => process.exit(0)));
