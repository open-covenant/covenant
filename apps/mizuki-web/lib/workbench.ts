import type { Bounty, Job, JobClass, Quote } from './types';
import { formatSolLamports, formatUsd } from './format';

export type WorkbenchAccount = {
  githubLogin: string;
  githubAvatarUrl?: string;
  displayName?: string;
  walletAddress?: string;
};

export type RepositoryReadiness = 'ready' | 'action_required' | 'unsupported' | 'checking';

export type WorkbenchRepository = {
  owner: string;
  repo: string;
  fullName: string;
  defaultBranch?: string;
  readiness: RepositoryReadiness;
  maintenanceAppInstalled: boolean;
  verifierAppInstalled: boolean;
  validationCommands: string[];
  eligibleIssueCount?: number;
  lastCheckedAt?: string;
  reason?: string;
  actionUrl?: string;
};

export type IssueEligibility = 'ready' | 'action_required' | 'unsupported' | 'checking';

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
  totalsScope?: 'account_lifetime' | 'latest_jobs';
  entries: BillingEntry[];
};

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
    const blockers = strings(source.blockers);
    const readiness: RepositoryReadiness =
      status === 'ready' || source.readyForWork === true
        ? 'ready'
        : status === 'unsupported'
          ? 'unsupported'
          : status === 'checking'
            ? 'checking'
            : 'action_required';
    const installation = record(source.installation);

    return [
      {
        owner,
        repo,
        fullName: `${owner}/${repo}`,
        defaultBranch: text(source.defaultBranch) ?? text(source.default_branch),
        readiness,
        maintenanceAppInstalled:
          bool(source.maintenanceAppInstalled) ??
          bool(source.coreAppInstalled) ??
          (text(core.status) ? ['ready', 'installed'].includes(text(core.status)!) : undefined) ??
          bool(core.installed) ??
          bool(core.ready) ??
          bool(installation.maintenance) ??
          false,
        verifierAppInstalled:
          bool(source.verifierAppInstalled) ??
          (text(policy.status)
            ? ['ready', 'installed'].includes(text(policy.status)!)
            : undefined) ??
          bool(policy.installed) ??
          bool(policy.ready) ??
          bool(installation.verifier) ??
          false,
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
    const eligibility: IssueEligibility =
      eligible === true
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
  const eligibilityCheck = record(checks.eligibility);
  const rawIssue = record(source.issue);
  const issue = normalizeIssues({ items: [rawIssue] })[0];
  if (!issue) throw new Error('The preflight response did not include an issue');
  const status = text(source.eligibility) ?? text(source.status);
  const checkedEligibility = text(eligibilityCheck.status);
  const readyForWork = bool(source.readyForWork) ?? bool(repositoryRecord.readyForWork);
  const eligibility: WorkbenchPreflight['eligibility'] =
    readyForWork === true
      ? 'ready'
      : readyForWork === false
        ? 'action_required'
        : status === 'ready'
          ? 'ready'
          : checkedEligibility === 'ready'
            ? 'ready'
            : status === 'unsupported'
              ? 'unsupported'
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

export function normalizeJobs(value: unknown): WorkbenchJob[] {
  return normalizeJobPage(value).jobs;
}

export function normalizeJobPage(value: unknown): WorkbenchJobPage {
  const source = record(value);
  return {
    jobs: listFrom<WorkbenchJob>(value, 'jobs'),
    limit: number(source.limit),
    truncated: bool(source.truncated) ?? false,
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
    totalsScope:
      source.totalsScope === 'account_lifetime' || source.totalsScope === 'latest_jobs'
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
  return !['delivered', 'rejected', 'failed', 'refunded'].includes(job.state);
}
