import { randomUUID } from 'node:crypto';
import type { GithubIssue, JobClass, Quote } from './types.js';

const REJECT = [
  /secret|credential|private key|\.env/i,
  /auth(?:entication|orization)?|oauth|login|password/i,
  /cryptograph|encrypt|signature|wallet|custod|transfer|payment/i,
  /deploy|production|infrastructure|terraform|kubernetes/i,
  /workflow|github action|license|vendored|generated/i,
];

const BLOCKED_LABEL = /(?:^|[\s:/_-])(?:enhancement|feature|security|vulnerability)(?:$|[\s:/_-])/i;
const FEATURE_PREFIX = /^\s*\[(?:enhancement|feat(?:ure)?(?: request)?)\]\s*/i;
const FEATURE_HEADING =
  /(?:^|\n)\s*(?:#{1,6}\s*|\*{1,2})?(?:enhancement|feature request|new feature|proposal)\b/i;
const FEATURE_TITLE =
  /^(?:\[[^\]]+\]\s*)?(?:(?:enhancement|feat(?:ure)?(?: request)?)\b|(?:add|build|create|implement|introduce|support|enable|expose|provide)\b)/i;
const NEW_CAPABILITY_TITLE =
  /^(?:\[[^\]]+\]\s*)?new\s+(?:[`'"]?[a-z][\w-]*[`'"]?\s+)?(?:command|subcommand|option|flag|endpoint|api|button|control|component|integration|provider|collector|parameter|argument|setting|field|mode|rule|workflow|capability|feature|functionality)\b/i;
const SAFE_MAINTENANCE_TITLE =
  /^(?:\[[^\]]+\]\s*)?(?:add|create|implement|provide)\b[^\n]{0,160}\b(?:tests?|test coverage|fixtures?|docs?|documentation|readme|type fixes?|lint fixes?)\b/i;
const NEW_CAPABILITY =
  /(?:^|\n)\s*(?:[-*]\s*)?(?:please\s+)?(?:add|build|create|implement|introduce|support|enable|expose|provide)\s+(?:(?:a|an|the)\s+)?(?:new\s+)?(?:cli\s+)?(?:[`'"]?(?:--?)?[a-z][\w-]*[`'"]?\s+)?(?:command|subcommand|option|flag|endpoint|api|button|control|component|integration|provider|collector|parameter|argument|setting|field|mode|rule|workflow|capability|feature|functionality)\b/i;
const NEW_CAPABILITY_SENTENCE =
  /\b(?:(?:we|you)\s+(?:should|need to|want to|would like to)|can\s+(?:we|you))\s+(?:add|build|create|implement|introduce|support|enable|expose|provide)\s+(?:(?:a|an|the)\s+)?(?:new\s+)?(?:cli\s+)?(?:[`'"]?(?:--?)?[a-z][\w-]*[`'"]?\s+)?(?:command|subcommand|option|flag|endpoint|api|button|control|component|integration|provider|collector|parameter|argument|setting|field|mode|rule|workflow|capability|feature|functionality)\b/i;

const MICRO = /\b(doc|docs|readme|typo|test|spec|fixture|config|configuration)\b/i;

export function createQuote(issue: GithubIssue, now = new Date()): Quote {
  if (issue.title.length > 256 || issue.body.length > 48_000) {
    throw new Error("issue is too large for Mizuki's bounded job scope");
  }
  assertMaintenanceScope(issue);
  const text = `${issue.title}\n${issue.body}`;
  const blocked = REJECT.find((pattern) => pattern.test(text));
  if (blocked) throw new Error("issue is outside Mizuki's safe MVP scope");
  const commands = validationCommands(issue.rootFiles, text);
  if (commands.length === 0) {
    throw new Error('repository has no supported deterministic validation command');
  }

  const jobClass: JobClass = MICRO.test(text) ? 'micro' : 'standard';
  return {
    id: randomUUID(),
    issueUrl: `https://github.com/${issue.owner}/${issue.repo}/issues/${issue.number}`,
    owner: issue.owner,
    repo: issue.repo,
    issueNumber: issue.number,
    issueTitle: issue.title,
    issueBody: issue.body,
    baseSha: issue.baseSha,
    defaultBranch: issue.defaultBranch,
    installationId: issue.installationId,
    authorizationReceipt: issue.authorizationReceipt,
    class: jobClass,
    priceAtomic: jobClass === 'micro' ? '2000000' : '10000000',
    maxFiles: jobClass === 'micro' ? 3 : 10,
    maxCostUsd: jobClass === 'micro' ? 0.8 : 4,
    validationCommands: commands,
    expiresAt: new Date(now.getTime() + 15 * 60_000).toISOString(),
  };
}

export function assertMaintenanceScope(
  issue: Pick<GithubIssue, 'title' | 'body' | 'labels'>,
): void {
  if (issue.labels.some((label) => BLOCKED_LABEL.test(label.trim()))) {
    throw new Error("issue labels place it outside Mizuki's maintenance-only scope");
  }
  const featureTitle =
    FEATURE_PREFIX.test(issue.title) ||
    NEW_CAPABILITY_TITLE.test(issue.title) ||
    NEW_CAPABILITY.test(issue.title) ||
    (FEATURE_TITLE.test(issue.title) && !SAFE_MAINTENANCE_TITLE.test(issue.title));
  if (
    featureTitle ||
    FEATURE_HEADING.test(issue.body) ||
    NEW_CAPABILITY.test(issue.body) ||
    NEW_CAPABILITY_SENTENCE.test(issue.body)
  ) {
    throw new Error("issue requests new capability outside Mizuki's maintenance-only scope");
  }
}

export function parseIssueUrl(value: string): { owner: string; repo: string; number: number } {
  const match = value.match(
    /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/issues\/(\d+)(?:[/?#].*)?$/,
  );
  if (!match) throw new Error('expected a public GitHub issue URL');
  return { owner: match[1]!, repo: match[2]!, number: Number(match[3]) };
}

function validationCommands(files: string[], issueText: string): string[] {
  const names = new Set(files);
  if (names.has('pnpm-lock.yaml') || names.has('package-lock.json') || names.has('yarn.lock')) {
    const docs = markdownTargets(issueText);
    if (docs.length > 0) {
      return [`npx --yes prettier@3.6.2 --check ${docs.join(' ')}`];
    }
  }
  if (names.has('pnpm-lock.yaml')) return ['pnpm test'];
  if (names.has('package-lock.json')) return ['npm test'];
  if (names.has('yarn.lock')) return ['yarn test'];
  if (names.has('Cargo.toml')) return ['cargo test'];
  if (names.has('pyproject.toml') || names.has('pytest.ini')) return ['pytest'];
  if (names.has('go.mod')) return ['go test ./...'];
  return [];
}

function markdownTargets(text: string): string[] {
  const targets = new Set<string>();
  const pattern =
    /(?:^|[\s`'"])((?:[A-Za-z0-9_.-]+\/)*[A-Za-z0-9_.-]+\.mdx?)(?=$|[\s`'",.:;!?)])/gi;
  for (const match of text.matchAll(pattern)) {
    const path = match[1]!;
    const segments = path.split('/');
    if (segments.some((segment) => segment === '.' || segment === '..')) continue;
    targets.add(path);
    if (targets.size === 3) break;
  }
  return [...targets];
}
