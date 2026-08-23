import { ConsensusUsdPriceOracle, HttpUsdPriceOracle, SolanaChainGateway } from './chain.js';
import { assertServerMode, loadConfig } from './config.js';
import { SignerMetrics } from './metrics.js';
import { GitHubMergeVerifier } from './github.js';
import { PolicyService } from './policy.js';
import { PostgresOperationStore } from './postgres.js';
import { RecoveryRunner } from './recovery.js';
import { createSignerServer } from './server.js';
import { startupReadinessPasses } from './startup.js';

const config = loadConfig();
assertServerMode(config);
const metrics = new SignerMetrics();
const store = new PostgresOperationStore(config.databaseUrl!);
const chain = new SolanaChainGateway({
  rpcUrl: config.rpcUrl!,
  secondaryRpcUrl: config.secondaryRpcUrl!,
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
  ),
  new HttpUsdPriceOracle(
    config.secondaryPriceUrl!,
    config.secondaryPriceToken,
    config.minSolUsdMicros,
    config.maxSolUsdMicros,
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

const server = createSignerServer({
  service: policy,
  store,
  metrics,
  authToken: config.authToken,
});
server.listen(config.port, config.host, () => {
  process.stdout.write(`mizuki policy signer listening on ${config.host}:${config.port}\n`);
  void recovery.run(100);
  recoveryInterval = setInterval(() => void recovery.run(), 5_000);
  recoveryInterval.unref();
});

async function shutdown(): Promise<void> {
  if (recoveryInterval) clearInterval(recoveryInterval);
  const closed = new Promise<void>((resolve) => server.close(() => resolve()));
  await recovery.active();
  await closed;
  await store.close();
}

process.once('SIGINT', () => void shutdown().finally(() => process.exit(0)));
process.once('SIGTERM', () => void shutdown().finally(() => process.exit(0)));
