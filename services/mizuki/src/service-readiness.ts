import { ensurePaymentCapacity } from './app.js';
import type { Config } from './config.js';
import { liveConfigIssues } from './config.js';
import type { UsePodContributorReviewer } from './contributor-reviewer.js';
import { MIN_RESCUE_BOUNTY_CENTS } from './domain/index.js';
import type { JobProcessor } from './executor.js';
import type { GithubClient } from './github.js';
import type { PaymentPolicy } from './policy-client.js';
import { refundProtectionEvidence, ServiceReadiness } from './readiness.js';
import type { MizukiStore } from './store.js';
import type { UpdaterStatusClient } from './updater-client.js';
import type { Payments } from './x402.js';

interface Dependencies {
  config: Config;
  store: MizukiStore;
  processor: Pick<JobProcessor, 'readiness'>;
  policy: PaymentPolicy;
  github: Pick<GithubClient, 'readiness'>;
  reviewer: Pick<UsePodContributorReviewer, 'readiness'>;
  updater?: Pick<UpdaterStatusClient, 'readiness'>;
  payments: Pick<Payments, 'readiness'>;
}

export function createServiceReadiness(deps: Dependencies): ServiceReadiness {
  const { config, store, processor, policy, github, reviewer, updater, payments } = deps;
  return new ServiceReadiness(
    {
      configuration: async () => ({ issues: liveConfigIssues(config) }),
      postgres: () => store.readiness(),
      operator_controls: async () => {
        await store.operatorControls();
      },
      coding_gateway: () => processor.readiness(),
      policy_signer: async () =>
        refundProtectionEvidence(
          await ensurePaymentCapacity({ config, store, policy }, 0n, MIN_RESCUE_BOUNTY_CENTS),
        ),
      github_app: () => github.readiness(),
      reviewer_route: () => reviewer.readiness(),
      updater: async () => {
        if (!updater) throw new Error('updater service is not configured');
        await updater.readiness();
      },
      x402_facilitator: () => payments.readiness(),
    },
    {
      refreshMs: config.readinessRefreshMs,
      maxAgeMs: config.readinessMaxAgeMs,
      timeoutMs: config.readinessTimeoutMs,
    },
  );
}
