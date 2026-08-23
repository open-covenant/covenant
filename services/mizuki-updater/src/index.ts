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
const operational = config.operational;
const github = operational
  ? new GitHubAppGateway({
      apiUrl: config.githubApiUrl,
      appId: operational.githubAppId,
      privateKey: operational.githubPrivateKey,
      repositories: config.allowedRepositories,
      timeoutMs: config.githubTimeoutMs,
      mergeMethod: config.githubMergeMethod,
      checkProducers: operational.checkProducers,
    })
  : undefined;
const deployments = operational
  ? new HttpDeploymentGateway({
      readinessUrl: operational.deployReadinessUrl,
      shadowUrl: operational.shadowHookUrl,
      shadowHealthUrlTemplate: operational.shadowHealthUrlTemplate,
      promotionHealthUrlTemplate: operational.promotionHealthUrlTemplate,
      promoteUrl: operational.promoteHookUrl,
      rollbackUrl: operational.rollbackHookUrl,
      token: operational.deployHookToken,
      timeoutMs: config.hookTimeoutMs,
    })
  : undefined;
const service =
  operational && github && deployments
    ? new UpdaterService(
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
        new ProposalVerifier({
          trustedProposalKeys: operational.trustedProposalKeys,
          trustedBenchmarkKeys: operational.trustedBenchmarkKeys,
          trustedReviewKeys: operational.trustedReviewKeys,
          allowedRepositories: config.allowedRepositories,
          allowedBaseBranches: config.allowedBaseBranches,
          headBranchPrefix: config.headBranchPrefix,
          mandatoryChecks: config.mandatoryChecks,
          maxProposalAgeMs: config.proposalMaxAgeMs,
        }),
        new HttpArtifactVerifier(
          config.artifactOrigins,
          config.artifactTimeoutMs,
          config.artifactMaxBytes,
        ),
        github,
        deployments,
        metrics,
      )
    : undefined;

await repository.migrate();
const server = createUpdaterServer({
  service,
  repository,
  metrics,
  authToken: config.authToken,
  readToken: config.readToken,
  operationalFailures: config.operationalFailures,
  operationalReadiness:
    github && deployments
      ? async () => {
          await Promise.all([github.readiness(), deployments.readiness()]);
        }
      : undefined,
});
server.headersTimeout = 10_000;
server.requestTimeout = 15_000;
server.keepAliveTimeout = 5_000;
server.listen(config.port, config.host, () => {
  process.stdout.write(`mizuki updater listening on ${config.host}:${config.port}\n`);
});

let recovering = false;
async function recover(): Promise<void> {
  if (recovering || !service) return;
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
