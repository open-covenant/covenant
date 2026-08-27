export type Loadable<T> =
  | { status: 'ready'; data: T; demo?: boolean }
  | { status: 'empty'; data: T; demo?: boolean }
  | { status: 'error'; error: string };

export type Admission = {
  intakeEnabled: boolean;
};

export type JobClass = 'micro' | 'standard';

export type JobState =
  | 'quoted'
  | 'settlement_pending'
  | 'payment_expired'
  | 'paid'
  | 'admitted'
  | 'running'
  | 'validating'
  | 'delivered'
  | 'rejected'
  | 'failed'
  | 'refund_pending'
  | 'refunded';

export type Quote = {
  id: string;
  issueUrl: string;
  owner: string;
  repo: string;
  issueNumber: number;
  issueTitle: string;
  class: JobClass;
  priceAtomic: string;
  maxFiles: number;
  maxCostUsd: number;
  expiresAt: string;
  payment?: unknown;
};

export type Validation = {
  command: string;
  exitCode: number;
};

export type ProviderRouteReceipt = {
  model: string;
  resolvedModel?: string;
  route: 'marketplace';
  providerId?: string;
  requestId?: string;
  costMicrounits?: string;
};

export type ReviewAttempt = {
  phase: 'implementation' | 'repair';
  status: 'pending' | 'received' | 'completed' | 'failed';
  artifactHash: string;
  reviewedAt: string;
  costUsd: number;
  attemptNumber?: number;
  maxAttempts?: number;
  maxCostUsd?: number;
  maxOutputTokens?: number;
  inputTokens?: number;
  outputTokens?: number;
  provider?: ProviderRouteReceipt;
  approved?: boolean;
  reason: string;
};

export type Job = {
  id: string;
  state: JobState;
  issueUrl: string;
  issueTitle?: string;
  class: JobClass;
  priceAtomic: string;
  paymentTransaction?: string;
  prUrl?: string;
  mergedAt?: string;
  deliveryEvidence?: {
    pullRequestNumber: number;
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    observedAt: string;
  };
  refundLiabilityDischarge?: {
    dischargedAt: string;
    evidenceHash: string;
  };
  refundTransaction?: string;
  error?: string;
  review?: {
    approved: boolean;
    reason: string;
    reviewedAt: string;
    artifactHash: string;
    provider?: ProviderRouteReceipt;
  };
  reviewAttempts?: ReviewAttempt[];
  changedFiles: string[];
  validations: Validation[];
  variableRouteCostEstimateUsd: number;
  costCoverage: CostCoverage;
  createdAt: string;
  updatedAt: string;
};

export type Metrics = {
  paidJobs: number;
  settlementPending: number;
  settlementPendingOldestSeconds: number | null;
  deliveredPrs: number;
  mergedPrs: number;
  refundCount: number;
  refundPending: number;
  refundPendingOldestSeconds: number | null;
  refundSuccessRate: number | null;
  externalRepositories: number;
  externalMaintainers: number;
  settledCustomerReceiptsUsd: number;
  recognizedRevenueUsd: number;
  platformReportedCreatorFeesSentLamports: string;
  variableRouteCostEstimateUsd: number;
  recognizedRevenueLessVariableRouteEstimateUsd: number;
  grossMarginStatus: 'unverified';
  costCoverage: CostCoverage;
  bountiesCreated: number;
  bountiesOpen: number;
  bountiesUnfundedOpen: number;
  bountiesClaimed: number;
  bountiesReleased: number;
  externalContributors: number;
  activeCapabilities: number;
  refundProtection: RefundProtection;
  recordedNetFlowUsd: number;
  plannedImprovementAllocationUsd: number;
  plannedResearchAllocationUsd: number;
  allocationTargetsSatisfied: boolean;
  tokenMint?: string | null;
  updatedAt: string;
};

export type CostCoverage = {
  included: readonly [
    'gateway_model_token_rate_estimate',
    'gateway_sandbox_runtime_estimate',
    'reviewer_model_token_rate_estimate',
  ];
  excluded: readonly [
    'provider_billing_adjustments',
    'chain_and_facilitator_fees',
    'infrastructure',
  ];
};

export type BountyState =
  | 'draft'
  | 'awaiting_funding'
  | 'funding'
  | 'open'
  | 'claimed'
  | 'pr_submitted'
  | 'validating'
  | 'claim_refund_pending'
  | 'offer_refund_pending'
  | 'release_refund_pending'
  | 'accepted'
  | 'released'
  | 'expired'
  | 'rejected'
  | 'disputed'
  | 'refunded';

export type Bounty = {
  id: string;
  title: string;
  repository: string;
  issueUrl: string;
  issueNumber?: number;
  amountUsd: number;
  amountAtomic?: string;
  asset?: string;
  state: BountyState;
  failureClass?: string;
  acceptanceCriteria: string[];
  claimExpiresAt?: string;
  claimant?: { github: string; wallet?: string };
  escrowTransaction?: string;
  releaseTransaction?: string;
  customerRefundTransaction?: string;
  escrowReturnTransaction?: string;
  pullRequestUrl?: string;
  review?: {
    approved: boolean;
    reason: string;
    reviewedAt: string;
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    inputTokens?: number;
    outputTokens?: number;
    provider?: ProviderRouteReceipt;
  };
  dispute?: {
    id: string;
    state: 'open' | 'release_pending' | 'refund_pending' | 'released' | 'refunded';
    openedAt: string;
    resolution?: {
      requestedDecision: 'release' | 'refund';
      settlementDecision: 'release' | 'refund';
      summary: string;
      references: string[];
      evidenceHash: string;
      decidedAt: string;
      resolvedAt?: string;
      transactionSignature?: string;
    };
  };
  createdAt: string;
  updatedAt: string;
  accountClaim?: {
    id: string;
    current: boolean;
    state:
      | 'active'
      | 'draft_submitted'
      | 'validating'
      | 'accepted'
      | 'released'
      | 'expired'
      | 'rejected'
      | 'disputed'
      | 'refunded';
    claimedAt: string;
    leaseExpiresAt: string;
    pullRequestUrl?: string;
    closedAt?: string;
  };
};

export type TreasuryAllocationBucket = {
  id: 'refund_target' | 'operating_target' | 'improvement_plan' | 'research_plan';
  label: string;
  allocatedUsd: number;
  targetUsd?: number;
};

export type LedgerEntry = {
  id: string;
  type:
    | 'customer_receipt'
    | 'platform_reported_creator_fee'
    | 'refund'
    | 'bounty_funding'
    | 'bounty_release'
    | 'bounty_return'
    | 'route_cost'
    | 'operating_cost'
    | 'refund_obligation'
    | 'treasury_deposit'
    | 'allocation';
  amountUsd?: number;
  amountAtomic?: string;
  asset?: string;
  direction: 'credit' | 'debit' | 'allocation';
  description: string;
  transaction?: string;
  occurredAt: string;
};

export type Treasury = {
  refundProtection: RefundProtection;
  recordedInflowsUsd: number;
  recordedOutflowsUsd: number;
  recordedNetFlowUsd: number;
  localOutstandingLiabilityUsd: number;
  plannedRunwayDays: number | null;
  allocationModel: {
    source: 'application_ledger';
    custodyVerified: false;
    modeledFundsUsd: number;
    targetsSatisfied: boolean;
    refundTargetUsd: number;
    refundAllocationUsd: number;
    operatingTargetUsd: number;
    operatingAllocationUsd: number;
    plannedImprovementAllocationUsd: number;
    plannedResearchAllocationUsd: number;
    policy: {
      refundTargetMinimumUsd: number;
      operatingTargetMinimumUsd: number;
      improvementShare: number;
      researchShare: number;
    };
    buckets: TreasuryAllocationBucket[];
  };
  ledger: LedgerEntry[];
  updatedAt: string;
};

export type RefundProtection = {
  status: 'verified' | 'degraded' | 'unavailable';
  source: 'policy_signer_finalized' | null;
  refundTreasury: string | null;
  refundMint: string | null;
  refundDecimals: number | null;
  finalizedBalanceAtomic: string | null;
  signerOutstandingLiabilityAtomic: string | null;
  unencumberedBalanceAtomic: string | null;
  newIntakeCapacityAtomic: string | null;
  remainingDailyLimitUsdCents: number | null;
  localOutstandingLiabilityAtomic: string;
  liabilityReconciled: boolean | null;
  liabilitiesBacked: boolean | null;
  checkedAt: string | null;
};

export type CapabilityState =
  | 'missing'
  | 'proposed'
  | 'funded'
  | 'implementing'
  | 'validating'
  | 'active'
  | 'degraded'
  | 'retired';

export type Capability = {
  id: string;
  name: string;
  description: string;
  state: CapabilityState;
  category: string;
  handoffUrl?: string;
  evidence?: {
    updaterAuditHash?: string;
    benchmarkReceiptId?: string;
    benchmarkReceiptHash?: string;
    reviewReceiptId?: string;
    reviewReceiptHash?: string;
    manifestHash?: string;
    artifactHash?: string;
    candidateSha?: string;
    pullRequestUrl?: string;
    deploymentId?: string;
    mergeSha?: string;
    promotionOperationId?: string;
    promotionHealthyAt?: string;
  };
  evidenceUrl?: string;
  benchmarkBefore?: number;
  benchmarkAfter?: number;
  benchmarkUnit?: string;
  activatedAt?: string;
  updatedAt: string;
};

export type ActivityKind =
  | 'job_paid'
  | 'job_delivered'
  | 'job_failed'
  | 'refund_started'
  | 'refund_pending'
  | 'refund_completed'
  | 'bounty_created'
  | 'bounty_creation_failed'
  | 'bounty_opened'
  | 'bounty_funded'
  | 'bounty_claimed'
  | 'bounty_pr_submitted'
  | 'bounty_accepted'
  | 'bounty_released'
  | 'bounty_expired'
  | 'bounty_disputed'
  | 'pull_request_merged'
  | 'escrow_released'
  | 'capability_proposed'
  | 'capability_activated'
  | 'capability_rolled_back'
  | 'rollback';

export type ActivityEvent = {
  id: string;
  kind: ActivityKind;
  title: string;
  description: string;
  amountUsd?: number;
  href?: string;
  transaction?: string;
  occurredAt: string;
};

export type Overview = {
  metrics: Loadable<Metrics>;
  bounties: Loadable<Bounty[]>;
  treasury: Loadable<Treasury>;
  capabilities: Loadable<Capability[]>;
  activity: Loadable<ActivityEvent[]>;
};
