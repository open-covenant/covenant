export type JobClass = 'micro' | 'standard';

export type JobState =
  | 'settlement_pending'
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
    contents: 'read';
    issues: 'read';
    metadata: 'read';
    pull_requests: 'read';
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
  route: 'marketplace';
  providerId?: string;
  requestId?: string;
  costMicrounits?: string;
};

export type ReviewAttempt = {
  id: string;
  phase: 'implementation' | 'repair';
  artifactHash: string;
  status?: 'pending' | 'received' | 'completed' | 'failed';
  provider?: ProviderRouteReceipt;
  costUsd: number;
  reviewedAt: string;
  inputTokens?: number;
  outputTokens?: number;
  approved?: boolean;
  reason?: string;
  error?: string;
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

export type OperatorControls = {
  intakeEnabled: boolean;
  claimsEnabled: boolean;
  revision: number;
  reason: string;
  updatedBy: string;
  updatedAt: string;
};

export type OperatorControlsPatch = {
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
