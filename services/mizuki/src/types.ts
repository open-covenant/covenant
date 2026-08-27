export type JobClass = 'micro' | 'standard';

export type JobState =
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
  issueBody: string;
  baseSha: string;
  defaultBranch: string;
  installationId?: number;
  authorizationReceipt?: GithubAuthorizationReceipt;
  class: JobClass;
  priceAtomic: string;
  maxFiles: number;
  maxCostUsd: number;
  validationCommands: string[];
  expiresAt: string;
};

export type GithubAuthorizationReceipt = {
  label: string;
  actorId: string;
  actorLogin: string;
  permission: 'triage' | 'write' | 'maintain' | 'admin';
  authorizedAt: string;
  verifiedAt: string;
  evidenceHash: string;
};

export type Payment = {
  payer: string;
  transaction: string;
  amountAtomic: string;
  signature?: string;
};

export const PAYMENT_ATTEMPT_STAGES = [
  'created',
  'wallet_opened',
  'wallet_signed',
  'submitting',
  'job_reserved',
  'expired_unpaid',
  'indeterminate',
] as const;

export type PaymentAttemptStage = (typeof PAYMENT_ATTEMPT_STAGES)[number];

export type CustomerPaymentAttempt = {
  id: string;
  githubId: string;
  quoteId: string;
  wallet: string;
  appBuild: string;
  idempotencyKey: string;
  stage: PaymentAttemptStage;
  retrySafe: boolean;
  expiresAt: string;
  paymentWindowEndUnixSeconds?: number;
  promptNonce?: string;
  promptAuthorizedAt?: string;
  serverAcceptedAt?: string;
  createdAt: string;
  updatedAt: string;
  jobId?: string;
  settlementTransaction?: string;
};

export type RepositoryAdmissionReceipt = {
  id: string;
  quoteId: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  reservationKeyHash: string;
  paymentAuthorizationHash: string;
  verifierAppId: string;
  installationId: number;
  repositorySelection: 'selected';
  permissions: {
    checks: 'read';
    contents: 'read';
    issues: 'read';
    metadata: 'read';
    pull_requests: 'read';
    statuses: 'read';
  };
  tokenRepositories: 1;
  tokenExpiresAt: string;
  admittedAt: string;
  evidenceHash: string;
};

export type ValidationResult = {
  command: string;
  exitCode: number;
  stdout: string;
  stderr: string;
};

export type RunArtifacts = {
  patch: string;
  changedFiles: string[];
  files: Array<{ path: string; content: string }>;
  validations: ValidationResult[];
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
  id: string;
  phase: 'implementation' | 'repair';
  artifactHash: string;
  attemptNumber?: number;
  maxAttempts?: number;
  maxCostUsd?: number;
  maxOutputTokens?: number;
  status?: 'pending' | 'received' | 'completed' | 'failed';
  provider?: ProviderRouteReceipt;
  costUsd: number;
  reviewedAt: string;
  inputTokens?: number;
  outputTokens?: number;
  approved?: boolean;
  reason?: string;
  error?: string;
  retryable?: boolean;
};

export type DeliveryEvidence = {
  pullRequestNumber: number;
  headSha: string;
  baseSha: string;
  baseRef: string;
  diffHash: string;
  observedAt: string;
};

export type Job = {
  id: string;
  idempotencyKey: string;
  paymentAttemptId?: string;
  quote: Quote;
  payment: Payment;
  repositoryAdmission?: RepositoryAdmissionReceipt;
  state: JobState;
  createdAt: string;
  updatedAt: string;
  runId?: string;
  prUrl?: string;
  mergedAt?: string;
  error?: string;
  refundTransaction?: string;
  refundOperationId?: string;
  paymentIntentId?: string;
  paymentWindowEndUnixSeconds?: number;
  refundLiabilityId?: string;
  refundLiabilityDischargedAt?: string;
  refundLiabilityDischargeEvidenceHash?: string;
  deliveryCommitSha?: string;
  deliveryEvidence?: DeliveryEvidence;
  reviewReceipt?: {
    approved: true;
    reason: string;
    reviewedAt: string;
    artifactHash: string;
    provider: ProviderRouteReceipt;
  };
  reviewAttempts?: ReviewAttempt[];
  artifacts?: RunArtifacts;
  inputTokens: number;
  outputTokens: number;
  reviewInputTokens?: number;
  reviewOutputTokens?: number;
  estimatedCostUsd: number;
  version: number;
};

export type ActivityKind =
  | 'job.paid'
  | 'job.delivered'
  | 'job.failed'
  | 'refund.pending'
  | 'refund.completed'
  | 'bounty.created'
  | 'bounty.creation_failed'
  | 'bounty.funded'
  | 'bounty.claimed'
  | 'bounty.pr_submitted'
  | 'bounty.accepted'
  | 'bounty.released'
  | 'bounty.expired'
  | 'bounty.disputed'
  | 'bounty.dispute_resolved'
  | 'capability.proposed'
  | 'capability.activated'
  | 'capability.rolled_back';

export type ActivityEvent = {
  id: string;
  kind: ActivityKind;
  subjectId: string;
  publicData: Record<string, unknown>;
  createdAt: string;
};

export type LedgerKind =
  | 'customer_payment'
  | 'route_cost'
  | 'refund_liability'
  | 'refund_completed'
  | 'bounty_reserved'
  | 'bounty_released'
  | 'bounty_returned'
  | 'creator_fee'
  | 'treasury_deposit'
  | 'operating_cost';

export type LedgerEntry = {
  id: string;
  kind: LedgerKind;
  referenceId: string;
  asset: string;
  amountAtomic: string;
  amountUsd: number;
  transaction?: string;
  createdAt: string;
};

export type Contributor = {
  githubId: string;
  githubLogin: string;
  wallet?: string;
  walletVerifiedAt?: string;
  createdAt: string;
  updatedAt: string;
};

export type AccountRepository = {
  githubId: string;
  owner: string;
  repo: string;
  repository: string;
  verifiedAt: string;
};

export const API_TOKEN_SCOPES = ['repositories:read', 'jobs:read', 'jobs:write'] as const;

export type ApiTokenScope = (typeof API_TOKEN_SCOPES)[number];

export type AccountApiToken = {
  id: string;
  githubId: string;
  name: string;
  prefix: string;
  tokenHash: string;
  scopes: ApiTokenScope[];
  expiresAt: string;
  createdAt: string;
  lastUsedAt?: string;
  revokedAt?: string;
};

export type WalletChallenge = {
  id: string;
  githubId: string;
  wallet: string;
  message: string;
  kind?: 'wallet_link' | 'bounty_bind';
  bountyId?: string;
  reservationId?: string;
  claimExpiresAt?: string;
  expiresAt: string;
  consumedAt?: string;
  createdAt: string;
};

export type GithubOAuthFlow = {
  id: string;
  binding: string;
  expiresAt: string;
  createdAt: string;
  consumedAt?: string;
};

export type OperatorControls = {
  intakeEnabled: boolean;
  claimsEnabled: boolean;
  revision: number;
  reason: string;
  updatedBy: string;
  updatedAt: string;
};

export type OperatorControlAuditEntry = OperatorControls & {
  expectedRevision: number;
};

export type OperatorControlsPatch = {
  expectedRevision: number;
  intakeEnabled?: boolean;
  claimsEnabled?: boolean;
  reason: string;
  updatedBy: string;
};

export type GithubIssue = {
  owner: string;
  repo: string;
  number: number;
  title: string;
  body: string;
  labels: string[];
  defaultBranch: string;
  baseSha: string;
  rootFiles: string[];
  installationId?: number;
  authorizationReceipt?: GithubAuthorizationReceipt;
};
