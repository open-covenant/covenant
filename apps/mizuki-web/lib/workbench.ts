import type { Bounty, Job, JobClass, Quote } from './types';
import { formatSolLamports, formatUsd } from './format';

export type WorkbenchAccount = {
  githubLogin: string;
  githubAvatarUrl?: string;
  displayName?: string;
  walletAddress?: string;
};

export type RepositoryReadiness =
  | 'ready'
  | 'action_required'
  | 'unavailable'
  | 'unsupported'
  | 'checking';

export type InstallationStatus = 'installed' | 'missing' | 'unavailable';

export type WorkbenchRepository = {
  owner: string;
  repo: string;
  fullName: string;
  defaultBranch?: string;
  readiness: RepositoryReadiness;
  maintenanceAppStatus: InstallationStatus;
  verifierAppStatus: InstallationStatus;
  validationCommands: string[];
  eligibleIssueCount?: number;
  lastCheckedAt?: string;
  reason?: string;
  actionUrl?: string;
};

export type IssueEligibility =
  | 'ready'
  | 'action_required'
  | 'unavailable'
  | 'unsupported'
  | 'checking';

export type WorkbenchIssue = {
  number: number;
  title: string;
  url: string;
  authorized: boolean;
  eligibility: IssueEligibility;
  class?: JobClass;
  reason?: string;
};

export type WorkbenchPreflight = {
  repository: string;
  issue: WorkbenchIssue;
  eligibility: Exclude<IssueEligibility, 'checking'>;
  class?: JobClass;
  maxFiles?: number;
  validationCommands: string[];
  capturedHeadSha?: string;
  reason?: string;
  quote?: Quote;
};

export type WorkbenchJob = Job & {
  owner?: string;
  repo?: string;
  issueNumber?: number;
  issueTitle?: string;
};

export type WorkbenchJobPage = {
  jobs: WorkbenchJob[];
  limit?: number;
  truncated: boolean;
  obligationCount: number;
};

export type WorkbenchPullRequest = {
  repository: string;
  number: number;
  title: string;
  url: string;
  state: 'open' | 'closed' | 'merged';
  draft: boolean;
  authorized: boolean;
  author?: string;
  headRef: string;
  headSha: string;
  baseRef: string;
  createdAt: string;
  updatedAt: string;
  provenance:
    | { kind: 'paid_job'; jobId: string; state: string }
    | { kind: 'bounty'; bountyId: string; state: string }
    | { kind: 'unlinked' };
};

export type WorkbenchPullRequestPage = {
  pullRequests: WorkbenchPullRequest[];
  truncated: boolean;
  unavailableRepositories: string[];
  checkedAt?: string;
};

export type BillingEntry = {
  id: string;
  kind: 'payment' | 'refund';
  state: 'pending' | 'finalized' | 'failed';
  amountAtomic: string;
  asset: string;
  jobId?: string;
  repository?: string;
  transaction?: string;
  occurredAt: string;
};

export type WorkbenchBilling = {
  walletAddress?: string;
  limit?: number;
  truncated: boolean;
  obligationCount: number;
  totalsScope?: 'account_lifetime' | 'latest_terminal_jobs_and_all_obligations';
  entries: BillingEntry[];
};

export const API_TOKEN_SCOPES = ['repositories:read', 'jobs:read', 'jobs:write'] as const;

export type ApiTokenScope = (typeof API_TOKEN_SCOPES)[number];

export type WorkbenchApiToken = {
  id: string;
  name: string;
  prefix: string;
  scopes: ApiTokenScope[];
  state: 'active' | 'expired' | 'revoked';
  expiresAt: string;
  createdAt: string;
  lastUsedAt?: string;
  revokedAt?: string;
};

export type ApiTokenCredential = {
  token: WorkbenchApiToken;
  secret: string;
};

export function workbenchAuthHref(returnTo: string): string {
  const destination = safeWorkbenchReturnPath(returnTo);
  return `/api/mizuki/v1/auth/github?return_to=${encodeURIComponent(destination)}`;
}

export function authorizePullRequestHref(pullRequest: WorkbenchPullRequest): string {
  const repository = parseRepositoryLocator(pullRequest.repository);
  if (!repository) return pullRequest.url;
  const returnTo = `/app/jobs/new?owner=${encodeURIComponent(repository.owner)}&repo=${encodeURIComponent(repository.repo)}`;
  return `/api/mizuki/v1/auth/github?${new URLSearchParams({
    return_to: returnTo,
    authorize_pr: pullRequest.url,
  })}`;
}

function safeWorkbenchReturnPath(value: string): string {
  try {
    const base = new URL('https://mizuki.invalid');
    const target = new URL(value, base);
    if (
      target.origin !== base.origin ||
      target.hash ||
      (target.pathname !== '/app' && !target.pathname.startsWith('/app/'))
    ) {
      return '/app';
    }
    target.searchParams.delete('auth_error');
    return `${target.pathname}${target.search}`;
  } catch {
    return '/app';
  }
}

export function githubAuthErrorMessage(value: string | null | undefined): string | undefined {
  switch (value) {
    case 'denied':
      return 'GitHub sign-in was cancelled. No account or repository access was changed.';
    case 'expired':
      return 'This GitHub sign-in request expired. Start a new sign-in to continue.';
    case 'incomplete':
      return 'GitHub returned an incomplete sign-in response. Start the sign-in again.';
    case 'invalid':
      return 'This GitHub sign-in response could not be verified. Start a new sign-in.';
    case 'permission':
      return 'GitHub could not confirm maintainer permission for that pull request. Check repository access and try again.';
    case 'replayed':
      return 'This GitHub sign-in request was already used. Start a new sign-in to continue.';
    case 'unavailable':
      return 'GitHub sign-in is temporarily unavailable. Try again shortly.';
    default:
      return undefined;
  }
}

type UnknownRecord = Record<string, unknown>;

function record(value: unknown): UnknownRecord {
  return typeof value === 'object' && value !== null ? (value as UnknownRecord) : {};
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function bool(value: unknown): boolean | undefined {
  return typeof value === 'boolean' ? value : undefined;
}

function number(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function strings(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => (text(item) ? [text(item)!] : []));
}

export function listFrom<T>(value: unknown, ...keys: string[]): T[] {
  if (Array.isArray(value)) return value as T[];
  const source = record(value);
  for (const key of ['items', 'data', ...keys]) {
    const candidate = source[key];
    if (Array.isArray(candidate)) return candidate as T[];
  }
  return [];
}

export function normalizeAccount(value: unknown): WorkbenchAccount {
  const root = record(value);
  const source = Object.keys(record(root.account)).length > 0 ? record(root.account) : root;
  const github = record(source.github);
  const user = record(source.user);
  const contributor = record(source.contributor);
  const login =
    text(github.login) ??
    text(user.githubLogin) ??
    text(contributor.githubLogin) ??
    text(source.githubLogin) ??
    text(source.login);

  if (!login) throw new Error('The account response did not include a GitHub login');

  return {
    githubLogin: login,
    githubAvatarUrl:
      text(github.avatarUrl) ??
      text(github.avatar_url) ??
      text(user.avatarUrl) ??
      text(source.avatarUrl),
    displayName: text(github.name) ?? text(user.name) ?? text(source.displayName),
    walletAddress:
      text(source.wallet) ??
      text(source.walletAddress) ??
      text(user.walletAddress) ??
      text(contributor.walletAddress),
  };
}

export function normalizeApiTokens(value: unknown): WorkbenchApiToken[] {
  return listFrom<unknown>(value, 'tokens').map(normalizeApiToken);
}

export function normalizeApiTokenCredential(value: unknown): ApiTokenCredential {
  const source = record(value);
  const secret = text(source.secret);
  if (!secret || !/^mzk_v1_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/.test(secret)) {
    throw new Error('The API token response did not include a valid one-time secret');
  }
  return { token: normalizeApiToken(source.token), secret };
}

export function normalizeCsrfToken(value: unknown): string {
  const csrfToken = text(record(value).csrfToken);
  if (!csrfToken || !/^[A-Za-z0-9_-]{43}$/.test(csrfToken)) {
    throw new Error('The security token response was invalid');
  }
  return csrfToken;
}

export function normalizeRepositories(value: unknown): WorkbenchRepository[] {
  return listFrom<unknown>(value, 'repositories').flatMap((item) => {
    const source = record(item);
    const rawFullName = text(source.fullName) ?? text(source.full_name) ?? text(source.repository);
    const [fullOwner, fullRepo] = rawFullName?.split('/') ?? [];
    const owner = text(source.owner) ?? fullOwner;
    const repo = text(source.repo) ?? text(source.name) ?? fullRepo;
    if (!owner || !repo) return [];

    const status = text(source.readiness) ?? text(source.status);
    const core = record(source.core);
    const policy = record(source.policy);
    const coreStatus = text(core.status);
    const policyStatus = text(policy.status);
    const blockers = strings(source.blockers);
    const readiness: RepositoryReadiness =
      status === 'unavailable' ||
      coreStatus === 'unavailable' ||
      policyStatus === 'unavailable' ||
      policyStatus === 'unknown'
        ? 'unavailable'
        : status === 'unsupported'
          ? 'unsupported'
          : status === 'checking'
            ? 'checking'
            : status === 'ready' || source.readyForWork === true
              ? 'ready'
              : status === 'action_required' ||
                  source.readyForWork === false ||
                  coreStatus === 'action_required' ||
                  policyStatus === 'action_required'
                ? 'action_required'
                : coreStatus === 'ready' && policyStatus === 'ready'
                  ? 'ready'
                  : 'unavailable';
    const installation = record(source.installation);

    return [
      {
        owner,
        repo,
        fullName: `${owner}/${repo}`,
        defaultBranch: text(source.defaultBranch) ?? text(source.default_branch),
        readiness,
        maintenanceAppStatus: normalizeInstallationStatus(
          text(source.maintenanceAppStatus) ?? text(source.coreAppStatus),
          bool(source.maintenanceAppInstalled) ?? bool(source.coreAppInstalled),
          coreStatus,
          bool(core.installed) ?? bool(core.ready) ?? bool(installation.maintenance),
        ),
        verifierAppStatus: normalizeInstallationStatus(
          text(source.verifierAppStatus),
          bool(source.verifierAppInstalled),
          policyStatus,
          bool(policy.installed) ?? bool(policy.ready) ?? bool(installation.verifier),
        ),
        validationCommands:
          strings(source.validationCommands).length > 0
            ? strings(source.validationCommands)
            : strings(source.commands),
        eligibleIssueCount:
          number(source.eligibleIssueCount) ?? number(source.eligible_issue_count),
        lastCheckedAt: text(source.lastCheckedAt) ?? text(source.checkedAt),
        reason:
          text(source.reason) ??
          text(source.message) ??
          (blockers.length ? blockers.join(' · ') : undefined),
        actionUrl: text(source.actionUrl) ?? text(source.installationUrl),
      },
    ];
  });
}

export function normalizeIssues(value: unknown): WorkbenchIssue[] {
  return listFrom<unknown>(value, 'issues').flatMap((item) => {
    const source = record(item);
    const issueNumber = number(source.number) ?? number(source.issueNumber);
    const title = text(source.title);
    const url = text(source.url) ?? text(source.issueUrl);
    if (issueNumber === undefined || !title || !url) return [];
    const authorized = bool(source.authorized) ?? bool(source.hasAuthorizationLabel) ?? false;
    const status = text(source.eligibility) ?? text(source.status);
    const eligible = bool(source.eligibility);
    const unavailable = status === 'unavailable' || bool(source.authorizationUnavailable) === true;
    const eligibility: IssueEligibility = unavailable
      ? 'unavailable'
      : eligible === true
        ? 'ready'
        : eligible === false
          ? 'action_required'
          : status === 'ready'
            ? 'ready'
            : status === 'unsupported'
              ? 'unsupported'
              : status === 'checking'
                ? 'checking'
                : authorized
                  ? 'ready'
                  : 'action_required';
    const jobClass = text(source.class);

    return [
      {
        number: issueNumber,
        title,
        url,
        authorized,
        eligibility,
        class: jobClass === 'micro' || jobClass === 'standard' ? jobClass : undefined,
        reason: text(source.reason) ?? text(source.message),
      },
    ];
  });
}

export function normalizePreflight(value: unknown): WorkbenchPreflight {
  const source = record(value);
  const repositoryRecord = record(source.repository);
  const checks = record(source.checks);
  const coreCheck = record(checks.core);
  const policyCheck = record(checks.policy);
  const maintainerCheck = record(checks.maintainer);
  const authorizationCheck = record(checks.authorization);
  const eligibilityCheck = record(checks.eligibility);
  const rawIssue = record(source.issue);
  const issue = normalizeIssues({ items: [rawIssue] })[0];
  if (!issue) throw new Error('The preflight response did not include an issue');
  const status = text(source.eligibility) ?? text(source.status);
  const checkedEligibility = text(eligibilityCheck.status);
  const checkStatuses = [
    text(coreCheck.status),
    text(policyCheck.status),
    text(maintainerCheck.status),
    text(authorizationCheck.status),
    checkedEligibility,
  ];
  const readyForWork = bool(source.readyForWork) ?? bool(repositoryRecord.readyForWork);
  const eligibility: WorkbenchPreflight['eligibility'] =
    status === 'unavailable' || checkStatuses.includes('unavailable')
      ? 'unavailable'
      : status === 'unsupported'
        ? 'unsupported'
        : readyForWork === true
          ? 'ready'
          : readyForWork === false
            ? 'action_required'
            : status === 'ready'
              ? 'ready'
              : checkedEligibility === 'ready'
                ? 'ready'
                : 'action_required';
  const jobClass = text(source.class) ?? text(rawIssue.class);
  const blockers = [...strings(source.blockers), ...strings(repositoryRecord.blockers)];
  const repositoryName =
    text(repositoryRecord.repository) ?? text(repositoryRecord.fullName) ?? text(source.repository);

  return {
    repository: repositoryName ?? new URL(issue.url).pathname.split('/').slice(1, 3).join('/'),
    issue,
    eligibility,
    class: jobClass === 'micro' || jobClass === 'standard' ? jobClass : undefined,
    maxFiles: number(source.maxFiles),
    validationCommands:
      strings(source.validationCommands).length > 0
        ? strings(source.validationCommands)
        : strings(repositoryRecord.validationCommands),
    capturedHeadSha: text(source.capturedHeadSha) ?? text(source.headSha),
    reason:
      text(source.reason) ??
      text(source.message) ??
      (blockers.length > 0 ? blockers.join(' · ') : undefined),
    quote: source.quote ? (source.quote as Quote) : undefined,
  };
}

function normalizeInstallationStatus(
  explicitStatus: string | undefined,
  explicitInstalled: boolean | undefined,
  checkStatus: string | undefined,
  legacyInstalled: boolean | undefined,
): InstallationStatus {
  if (
    explicitStatus === 'installed' ||
    explicitStatus === 'missing' ||
    explicitStatus === 'unavailable'
  ) {
    return explicitStatus;
  }
  if (checkStatus === 'unavailable' || checkStatus === 'unknown' || checkStatus === 'checking') {
    return 'unavailable';
  }
  if (checkStatus === 'ready' || checkStatus === 'installed') return 'installed';
  if (
    checkStatus === 'action_required' ||
    checkStatus === 'missing' ||
    checkStatus === 'required' ||
    checkStatus === 'not_installed'
  ) {
    return 'missing';
  }
  if (explicitInstalled !== undefined) return explicitInstalled ? 'installed' : 'missing';
  if (legacyInstalled !== undefined) return legacyInstalled ? 'installed' : 'missing';
  return 'unavailable';
}

function normalizeApiToken(value: unknown): WorkbenchApiToken {
  const source = record(value);
  const id = text(source.id);
  const name = text(source.name);
  const prefix = text(source.prefix);
  const expiresAt = text(source.expiresAt);
  const createdAt = text(source.createdAt);
  const lastUsedAt = text(source.lastUsedAt);
  const revokedAt = text(source.revokedAt);
  const state = text(source.state);
  const scopes = strings(source.scopes);
  if (
    !id?.match(/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i) ||
    !name ||
    name.length > 80 ||
    !prefix?.match(/^mzk_v1_[A-Za-z0-9_-]{12}$/) ||
    !validIsoTimestamp(expiresAt) ||
    !validIsoTimestamp(createdAt) ||
    Date.parse(expiresAt) <= Date.parse(createdAt) ||
    (lastUsedAt !== undefined && !validIsoTimestamp(lastUsedAt)) ||
    (lastUsedAt !== undefined && Date.parse(lastUsedAt) < Date.parse(createdAt)) ||
    (revokedAt !== undefined && !validIsoTimestamp(revokedAt)) ||
    (revokedAt !== undefined && Date.parse(revokedAt) < Date.parse(createdAt)) ||
    (state === 'revoked' && !revokedAt) ||
    (state !== 'revoked' && revokedAt !== undefined) ||
    (state !== 'active' && state !== 'expired' && state !== 'revoked') ||
    scopes.length === 0 ||
    new Set(scopes).size !== scopes.length ||
    !scopes.every((scope): scope is ApiTokenScope =>
      API_TOKEN_SCOPES.includes(scope as ApiTokenScope),
    )
  ) {
    throw new Error('The API token response is incomplete');
  }
  return {
    id,
    name,
    prefix,
    scopes,
    state,
    expiresAt,
    createdAt,
    lastUsedAt,
    revokedAt,
  };
}

function validIsoTimestamp(value: string | undefined): value is string {
  return Boolean(
    value &&
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value) &&
    Number.isFinite(Date.parse(value)),
  );
}

export function normalizeJobs(value: unknown): WorkbenchJob[] {
  return normalizeJobPage(value).jobs;
}

export function normalizeJobPage(value: unknown): WorkbenchJobPage {
  const source = record(value);
  return {
    jobs: listFrom<WorkbenchJob>(value, 'jobs'),
    limit: number(source.limit),
    truncated: bool(source.truncated) ?? false,
    obligationCount: number(source.obligationCount) ?? 0,
  };
}

export function normalizeJob(value: unknown): WorkbenchJob {
  const source = record(value);
  const candidate = Object.keys(record(source.job)).length > 0 ? record(source.job) : source;
  if (!text(candidate.id) || !text(candidate.issueUrl) || !text(candidate.state)) {
    throw new Error('The job response is incomplete');
  }
  return candidate as WorkbenchJob;
}

export function normalizePullRequestPage(value: unknown): WorkbenchPullRequestPage {
  const source = record(value);
  const pullRequests = listFrom<unknown>(value, 'pullRequests').flatMap((item) => {
    const pull = record(item);
    const provenance = record(pull.provenance);
    const repository = text(pull.repository);
    const pullNumber = number(pull.number);
    const title = text(pull.title);
    const url = text(pull.url);
    const state = text(pull.state);
    const headRef = text(pull.headRef);
    const headSha = text(pull.headSha);
    const baseRef = text(pull.baseRef);
    const createdAt = text(pull.createdAt);
    const updatedAt = text(pull.updatedAt);
    if (
      !repository ||
      pullNumber === undefined ||
      !title ||
      !url ||
      !headRef ||
      !headSha ||
      !baseRef ||
      !createdAt ||
      !updatedAt ||
      (state !== 'open' && state !== 'closed' && state !== 'merged')
    ) {
      return [];
    }

    const kind = text(provenance.kind);
    const jobId = text(provenance.jobId);
    const bountyId = text(provenance.bountyId);
    const normalizedState: WorkbenchPullRequest['state'] = state;
    const normalizedProvenance: WorkbenchPullRequest['provenance'] =
      kind === 'paid_job' && jobId
        ? { kind, jobId, state: text(provenance.state) ?? 'unknown' }
        : kind === 'bounty' && bountyId
          ? {
              kind,
              bountyId,
              state: text(provenance.state) ?? 'unknown',
            }
          : { kind: 'unlinked' };
    return [
      {
        repository,
        number: pullNumber,
        title,
        url,
        state: normalizedState,
        draft: bool(pull.draft) ?? false,
        authorized: bool(pull.authorized) ?? false,
        ...(text(pull.author) ? { author: text(pull.author) } : {}),
        headRef,
        headSha,
        baseRef,
        createdAt,
        updatedAt,
        provenance: normalizedProvenance,
      },
    ];
  });
  return {
    pullRequests,
    truncated: bool(source.truncated) ?? false,
    unavailableRepositories: strings(source.unavailableRepositories),
    checkedAt: text(source.checkedAt),
  };
}

export function normalizeBilling(value: unknown): WorkbenchBilling {
  const source = record(value);
  const rawEntries = listFrom<unknown>(value, 'entries', 'transactions');
  const combined =
    rawEntries.length > 0
      ? rawEntries
      : [
          ...listFrom<unknown>(source.payments, 'payments'),
          ...listFrom<unknown>(source.refunds, 'refunds'),
        ];

  const entries = combined.flatMap((item, index) => {
    const entry = record(item);
    const rawKind = text(entry.kind) ?? text(entry.type) ?? '';
    const kind: BillingEntry['kind'] = rawKind.includes('refund') ? 'refund' : 'payment';
    const rawState = text(entry.state) ?? text(entry.status);
    const state: BillingEntry['state'] =
      rawState === 'failed'
        ? 'failed'
        : rawState === 'finalized' || rawState === 'completed' || rawState === 'confirmed'
          ? 'finalized'
          : 'pending';
    const amountAtomic =
      text(entry.amountAtomic) ?? text(entry.priceAtomic) ?? text(entry.amount) ?? '0';
    const occurredAt =
      text(entry.occurredAt) ?? text(entry.createdAt) ?? text(entry.updatedAt) ?? '';

    return [
      {
        id: text(entry.id) ?? `${kind}-${index}`,
        kind,
        state,
        amountAtomic,
        asset: text(entry.asset) ?? 'USDC',
        jobId: text(entry.jobId),
        repository: text(entry.repository),
        transaction: text(entry.transaction) ?? text(entry.signature),
        occurredAt,
      },
    ];
  });

  return {
    walletAddress: text(source.walletAddress) ?? text(source.payerWallet),
    limit: number(source.limit),
    truncated: bool(source.truncated) ?? false,
    obligationCount: number(source.obligationCount) ?? 0,
    totalsScope:
      source.totalsScope === 'account_lifetime' ||
      source.totalsScope === 'latest_terminal_jobs_and_all_obligations'
        ? source.totalsScope
        : undefined,
    entries: entries.sort(
      (left, right) => Date.parse(right.occurredAt) - Date.parse(left.occurredAt),
    ),
  };
}

export function normalizeBounties(value: unknown): Bounty[] {
  return listFrom<Bounty>(value, 'bounties');
}

export function normalizeBounty(value: unknown): Bounty {
  const source = record(value);
  const candidate = Object.keys(record(source.bounty)).length > 0 ? record(source.bounty) : source;
  if (!text(candidate.id) || !text(candidate.title) || !text(candidate.state)) {
    throw new Error('The bounty response is incomplete');
  }
  return candidate as unknown as Bounty;
}

export function bountyPayoutText(amountAtomic: string | undefined, amountUsd: number) {
  let exact = 'Exact SOL amount unavailable';
  if (amountAtomic) {
    try {
      exact = formatSolLamports(amountAtomic);
    } catch {
      // Malformed public data must not be presented as an exact on-chain amount.
    }
  }

  return {
    exact,
    approximate: `Approx. ${formatUsd(amountUsd)}`,
  };
}

export function parseRepositoryLocator(value: string): { owner: string; repo: string } | undefined {
  const trimmed = value.trim().replace(/\.git$/, '');
  if (!trimmed) return undefined;
  const direct = trimmed.match(/^([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)$/);
  if (direct) return { owner: direct[1], repo: direct[2] };
  try {
    const url = new URL(trimmed);
    if (url.protocol !== 'https:' || url.hostname.toLowerCase() !== 'github.com') return undefined;
    const parts = url.pathname.split('/').filter(Boolean);
    if (parts.length !== 2) return undefined;
    if (!/^[A-Za-z0-9_.-]+$/.test(parts[0]) || !/^[A-Za-z0-9_.-]+$/.test(parts[1])) {
      return undefined;
    }
    return { owner: parts[0], repo: parts[1] };
  } catch {
    return undefined;
  }
}

export function jobRepository(job: WorkbenchJob): string {
  if (job.owner && job.repo) return `${job.owner}/${job.repo}`;
  try {
    return new URL(job.issueUrl).pathname.split('/').slice(1, 3).join('/');
  } catch {
    return 'Public repository';
  }
}

export function jobIssueNumber(job: WorkbenchJob): number | undefined {
  if (job.issueNumber !== undefined) return job.issueNumber;
  try {
    const value = new URL(job.issueUrl).pathname.match(/\/issues\/(\d+)/)?.[1];
    return value ? Number(value) : undefined;
  } catch {
    return undefined;
  }
}

export function isActiveJob(job: WorkbenchJob): boolean {
  return !['delivered', 'refunded'].includes(job.state);
}
