import { createServer } from 'node:http';
import { createApp, SerialGate } from './app.js';
import { ContributorAuth } from './auth.js';
import { BountyService } from './bounties.js';
import { CapabilityService } from './capabilities.js';
import { ClawPumpClient, EarningsReconciler } from './clawpump.js';
import { assertBootConfig, loadConfig } from './config.js';
import { JobProcessor } from './executor.js';
import { GithubClient } from './github.js';
import { finalizeJobMerge } from './merges.js';
import { UsePodContributorReviewer } from './contributor-reviewer.js';
import { MemoryStore, PostgresStore } from './store.js';
import { Payments, USDC_DECIMALS, USDC_MAINNET } from './x402.js';
import { PolicySignerClient, refundLiabilityCommitment } from './policy-client.js';
import { recordPaymentReceipts } from './receipts.js';
import { GithubWebhookHandler } from './webhooks.js';
import { UpdaterStatusClient } from './updater-client.js';
import { createServiceReadiness } from './service-readiness.js';

const config = loadConfig();
assertBootConfig(config);
const store = config.databaseUrl
  ? await PostgresStore.connect(config.databaseUrl)
  : new MemoryStore();
const github = new GithubClient(config);
const payments = new Payments(config);
const policy = new PolicySignerClient(config);
const paymentAdmission = new SerialGate();
const reviewer = new UsePodContributorReviewer(config, store, github);
const bounties = new BountyService(store, policy, reviewer, undefined, config);
const updater =
  config.updaterUrl && config.updaterToken
    ? new UpdaterStatusClient(config.updaterUrl, config.updaterToken, config.updaterTimeoutMs)
    : undefined;
const capabilities = new CapabilityService(store, updater);
const earnings = new EarningsReconciler(store, new ClawPumpClient(config));
const processor = new JobProcessor(
  config,
  store,
  github,
  fetch,
  async (job) => {
    await Promise.all([bounties.createAfterRefund(job), capabilities.recordFailure(job)]);
  },
  policy,
);
const readiness = createServiceReadiness({
  config,
  store,
  processor,
  policy,
  github,
  reviewer,
  updater,
  payments,
});
const auth = new ContributorAuth(config, store, fetch, policy);
const webhooks = new GithubWebhookHandler(store, async (payload) => {
  if (
    payload.action !== 'closed' ||
    !payload.pull_request.merged ||
    !payload.pull_request.merged_at
  ) {
    return;
  }
  const job = (await store.jobsList()).find(
    (candidate) => candidate.prUrl === payload.pull_request.html_url,
  );
  if (job) {
    await finalizeJobMerge(store, policy, config.paymentMode, job, payload.pull_request.merged_at);
  }
  const bounty = (await store.bountiesList()).find(
    (candidate) => candidate.activeClaim?.draftPullRequestUrl === payload.pull_request.html_url,
  );
  if (bounty) {
    await bounties.releaseMerged(bounty.id, payload.pull_request.html_url);
  }
});
const server = createServer(
  createApp({
    config,
    store,
    github,
    payments,
    processor,
    auth,
    webhooks,
    bounties,
    policy,
    paymentAdmission,
    readiness,
  }),
);

server.listen(config.port, config.host, () => {
  console.log(
    `Mizuki listening on http://${config.host}:${config.port} (${config.paymentMode} payments)`,
  );
});

let financialRefreshRunning = false;
let capabilityRefreshRunning = false;

const mergePoll = setInterval(() => void refreshMerges(), 5 * 60_000);
mergePoll.unref();
void refreshMerges();
const bountyPoll = setInterval(() => void refreshBounties(), 60_000);
bountyPoll.unref();
void refreshBounties();
const financialPoll = setInterval(() => void refreshFinancialOperations(), 30_000);
financialPoll.unref();
void refreshFinancialOperations();
const earningsPoll = setInterval(() => void refreshEarnings(), 15 * 60_000);
earningsPoll.unref();
void refreshEarnings();
const capabilityPoll = setInterval(() => void refreshCapabilities(), config.updaterPollIntervalMs);
capabilityPoll.unref();
void refreshCapabilities();

async function refreshMerges(): Promise<void> {
  let jobs;
  try {
    jobs = await store.jobsList();
  } catch (cause) {
    console.error(
      `merge refresh failed: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
    return;
  }

  for (const job of jobs) {
    if (job.state !== 'delivered' || !job.prUrl || job.refundLiabilityDischargedAt) continue;
    try {
      const mergedAt = job.mergedAt ?? (await github.mergedAt(job));
      if (mergedAt) {
        await finalizeJobMerge(store, policy, config.paymentMode, job, mergedAt);
      }
    } catch (cause) {
      console.error(
        `merge refresh failed for job ${job.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
  }
}

async function refreshBounties(): Promise<void> {
  try {
    for (const job of await store.jobsList()) {
      if (job.state !== 'refunded') continue;
      await Promise.all([bounties.createAfterRefund(job), capabilities.recordFailure(job)]);
    }
    await refreshMergedBounties();
    await bounties.expireOffers();
    await bounties.expireClaims();
    await bounties.fundAwaiting();
    const recovery = await bounties.reconcileFinancialOperations();
    if (recovery.failed > 0) {
      console.error(`bounty financial recovery has ${recovery.failed} pending operation(s)`);
    }
  } catch (cause) {
    console.error(
      `bounty refresh failed: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
  }
}

async function refreshMergedBounties(): Promise<void> {
  const jobs = new Map((await store.jobsList()).map((job) => [job.id, job]));
  for (const bounty of await store.bountiesList()) {
    const pullRequestUrl = bounty.activeClaim?.draftPullRequestUrl;
    if (!pullRequestUrl || !['pr_submitted', 'validating', 'accepted'].includes(bounty.state)) {
      continue;
    }
    const installationId = jobs.get(bounty.sourceJobId)?.quote.installationId;
    if (!installationId) continue;
    try {
      const mergedAt = await github.pullRequestMergedAt(pullRequestUrl, installationId);
      if (mergedAt) await bounties.releaseMerged(bounty.id, pullRequestUrl);
    } catch (cause) {
      console.error(
        `bounty merge refresh failed for ${bounty.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
  }
}

async function refreshFinancialOperations(): Promise<void> {
  if (financialRefreshRunning) return;
  financialRefreshRunning = true;
  try {
    for (const job of await store.jobsList()) {
      if (job.state === 'settlement_pending') {
        try {
          const paid = await paymentAdmission.run(async () => {
            const payment = await payments.retrySettlement(job.quote, job.payment);
            const liability =
              config.paymentMode === 'live'
                ? await policy.registerRefundLiability(
                    job.id,
                    payment.transaction,
                    refundLiabilityCommitment(job.quote),
                  )
                : undefined;
            if (liability) {
              const commitment = refundLiabilityCommitment(job.quote);
              if (
                liability.jobId !== job.id ||
                liability.settlementSignature !== payment.transaction ||
                liability.payer !== payment.payer ||
                liability.mint !== USDC_MAINNET ||
                liability.decimals !== USDC_DECIMALS ||
                liability.rawAmount !== payment.amountAtomic ||
                liability.amountUsdCents !== Number(payment.amountAtomic) / 10_000 ||
                liability.repository !== commitment.repository ||
                liability.issueNumber !== commitment.issueNumber ||
                liability.baseRef !== commitment.baseRef ||
                liability.baseSha !== commitment.baseSha ||
                liability.repositoryAuthorizedAt !== commitment.repositoryAuthorizedAt ||
                liability.authorizationEvidenceHash !== commitment.authorizationEvidenceHash
              ) {
                throw new Error('refund liability evidence does not match the recovered payment');
              }
            }
            if (liability) await store.patchJob(job.id, { refundLiabilityId: liability.id });
            return store.transitionJob(job.id, 'settlement_pending', 'paid', {
              payment,
              refundLiabilityId: liability?.id,
            });
          });
          await recordPaymentReceipts(store, paid);
          void processor.process(job.id);
        } catch (cause) {
          console.error(
            `settlement recovery failed for job ${job.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
          );
        }
        continue;
      }
      try {
        await recordPaymentReceipts(store, job);
      } catch (cause) {
        console.error(
          `payment receipt recovery failed for job ${job.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
        );
      }
      if (job.state === 'paid') void processor.process(job.id);
    }
    const refunds = await processor.reconcileRefunds();
    if (refunds.pending > 0) {
      console.error(`${refunds.pending} refund operation(s) remain pending`);
    }
    await processor.reconcileInFlight();
    await processor.reconcileReceipts();
  } catch (cause) {
    console.error(
      `financial recovery failed: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
  } finally {
    financialRefreshRunning = false;
  }
}

async function refreshEarnings(): Promise<void> {
  try {
    await earnings.reconcile();
  } catch (cause) {
    console.error(
      `creator fee reconciliation failed: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
  }
}

async function refreshCapabilities(): Promise<void> {
  if (capabilityRefreshRunning) return;
  capabilityRefreshRunning = true;
  try {
    const result = await capabilities.reconcileUpdater();
    if (result.failed > 0) {
      console.error(`capability reconciliation failed for ${result.failed} upgrade(s)`);
    }
  } catch (cause) {
    console.error(
      `capability reconciliation failed: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
  } finally {
    capabilityRefreshRunning = false;
  }
}

let closing = false;
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => void shutdown(signal));
}

async function shutdown(signal: NodeJS.Signals): Promise<void> {
  if (closing) return;
  closing = true;
  clearInterval(mergePoll);
  clearInterval(bountyPoll);
  clearInterval(financialPoll);
  clearInterval(earningsPoll);
  clearInterval(capabilityPoll);
  await new Promise<void>((resolve) => server.close(() => resolve()));
  await store.close();
  console.log(`Mizuki stopped after ${signal}`);
}
