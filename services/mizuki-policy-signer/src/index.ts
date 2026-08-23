import { ConsensusUsdPriceOracle, HttpUsdPriceOracle, SolanaChainGateway } from './chain.js';
import { assertServerMode, loadConfig } from './config.js';
import { SignerMetrics } from './metrics.js';
import { GitHubMergeVerifier } from './github.js';
import { PolicyService } from './policy.js';
import { PostgresOperationStore } from './postgres.js';
import { RecoveryRunner, shutdownResources, waitForShutdown } from './recovery.js';
import { createSignerServer } from './server.js';
import { startupReadinessPasses } from './startup.js';

const config = loadConfig();
assertServerMode(config);
const metrics = new SignerMetrics();
const store = new PostgresOperationStore(config.databaseUrl!);
const chain = new SolanaChainGateway({
  rpcUrl: config.rpcUrl!,
  secondaryRpcUrl: config.secondaryRpcUrl!,
  rpcTimeoutMs: config.rpcTimeoutMs,
  refundPrivateKeyJson: config.refundPrivateKeyJson!,
  escrowPrivateKeyJson: config.escrowPrivateKeyJson!,
  refundTreasury: config.refundTreasury!,
  escrowAuthority: config.escrowAuthority!,
  refundMint: config.refundMint!,
  refundDecimals: config.refundDecimals,
  refundTokenProgram: config.refundTokenProgram,
  escrowProgramId: config.escrowProgramId!,
  escrowProgramDataSha256: config.escrowProgramDataSha256!,
  solFeeReserveLamports: config.solFeeReserveLamports,
});
const prices = new ConsensusUsdPriceOracle(
  new HttpUsdPriceOracle(
    config.priceUrl!,
    config.priceToken,
    config.minSolUsdMicros,
    config.maxSolUsdMicros,
    config.maxPriceAgeMs,
  ),
  new HttpUsdPriceOracle(
    config.secondaryPriceUrl!,
    config.secondaryPriceToken,
    config.minSolUsdMicros,
    config.maxSolUsdMicros,
    config.maxPriceAgeMs,
  ),
  config.maxPriceDivergenceBps,
);
const merges = new GitHubMergeVerifier(config.githubToken!);

await store.migrate();
const policy = new PolicyService(
  {
    refundTreasury: config.refundTreasury ?? '11111111111111111111111111111111',
    escrowAuthority: config.escrowAuthority ?? '11111111111111111111111111111111',
    refundMint: config.refundMint ?? '11111111111111111111111111111111',
    refundDecimals: config.refundDecimals,
    jobAuthorityPublicKey: config.jobAuthorityPublicKey!,
    refundAuthMaxTtlSeconds: config.refundAuthMaxTtlSeconds,
    refundLiabilityMaxAgeSeconds: config.refundLiabilityMaxAgeSeconds,
    operationLimitUsdCents: config.operationLimitUsdCents,
    refundDailyLimitUsdCents: config.refundDailyLimitUsdCents,
    escrowDailyLimitUsdCents: config.escrowDailyLimitUsdCents,
    maxEscrowLamports: config.maxEscrowLamports,
    solFeeReserveLamports: config.solFeeReserveLamports,
    bindChallengeTtlSeconds: config.bindChallengeTtlSeconds,
    githubGrantTtlSeconds: config.githubGrantTtlSeconds,
    claimTtlSeconds: config.claimTtlSeconds,
  },
  store,
  chain,
  prices,
  merges,
  metrics,
);

const startupReady = await startupReadinessPasses(() => policy.probeReadiness());
if (!startupReady) {
  process.stderr.write('mizuki policy signer startup readiness is degraded\n');
}
const recovery = new RecoveryRunner(
  (limit) => policy.recover(limit),
  () => {
    metrics.increment('errors');
    process.stderr.write('mizuki policy signer recovery failed\n');
  },
);
let recoveryInterval: ReturnType<typeof setInterval> | undefined;
let shuttingDown = false;
let shutdownTask: Promise<boolean> | null = null;

const server = createSignerServer({
  service: policy,
  store,
  metrics,
  authToken: config.authToken,
});
server.listen(config.port, config.host, () => {
  process.stdout.write(`mizuki policy signer listening on ${config.host}:${config.port}\n`);
  if (shuttingDown) return;
  void recovery.run(100);
  recoveryInterval = setInterval(() => void recovery.run(), 5_000);
  recoveryInterval.unref();
});

function shutdown(): Promise<boolean> {
  if (shutdownTask) return shutdownTask;
  shuttingDown = true;
  if (recoveryInterval) clearInterval(recoveryInterval);
  const closed = new Promise<void>((resolve) => server.close(() => resolve()));
  shutdownTask = shutdownResources(
    recovery.active(),
    async (force) => {
      if (force) server.closeAllConnections();
      await closed;
    },
    () => store.close(),
  ).then((clean) => {
    if (!clean) {
      process.stderr.write(
        'mizuki policy signer recovery shutdown grace expired; database close skipped\n',
      );
    }
    return clean;
  });
  return shutdownTask;
}

async function shutdownAndExit(): Promise<void> {
  try {
    const clean = await waitForShutdown(shutdown());
    if (!clean) process.stderr.write('mizuki policy signer shutdown did not complete cleanly\n');
    process.exit(clean ? 0 : 1);
  } catch {
    process.stderr.write('mizuki policy signer shutdown failed\n');
    process.exit(1);
  }
}

process.once('SIGINT', () => void shutdownAndExit());
process.once('SIGTERM', () => void shutdownAndExit());
