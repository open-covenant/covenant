/**
 * Assess whether a public GitHub repository qualifies for Mizuki's automated
 * maintenance, and report the command Mizuki would run to validate a change.
 *
 * The assessment is deliberately read-only and content-free about the work
 * itself: it observes the repository's root manifests and reports what follows
 * from them. It is not a quote, does not reserve anything, and does not claim
 * an issue is fixable.
 */

const GITHUB_API = 'https://api.github.com';
const MAX_ROOT_ENTRIES = 300;

export class MaintenanceLookupError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

export interface MaintenanceAssessment {
  repository: string;
  observedAt: string;
  eligible: boolean;
  reason?: string;
  defaultBranch?: string;
  validationCommand?: string;
  detectedManifest?: string;
  supportedManifests: string[];
}

/**
 * Mirrors the manifest detection in services/mizuki (quote.ts). Kept as a copy
 * because this service installs without the workspace; the supported set is
 * part of Mizuki's public contract, so a drift is a documentation bug rather
 * than a silent behaviour change.
 */
const MANIFEST_COMMANDS: Array<[string, string]> = [
  ['pnpm-lock.yaml', 'pnpm test'],
  ['package-lock.json', 'npm test'],
  ['yarn.lock', 'yarn test'],
  ['Cargo.toml', 'cargo test'],
  ['pyproject.toml', 'pytest'],
  ['pytest.ini', 'pytest'],
  ['go.mod', 'go test ./...'],
];

export const SUPPORTED_MANIFESTS = MANIFEST_COMMANDS.map(([name]) => name);

export function validationCommandFor(rootFiles: readonly string[]): [string, string] | undefined {
  const names = new Set(rootFiles);
  return MANIFEST_COMMANDS.find(([manifest]) => names.has(manifest));
}

export function isRepositoryName(value: string): boolean {
  return /^[A-Za-z0-9._-]{1,100}$/.test(value) && value !== '.' && value !== '..';
}

async function githubJson(path: string, token: string | undefined, timeoutMs: number) {
  const response = await fetch(`${GITHUB_API}${path}`, {
    headers: {
      accept: 'application/vnd.github+json',
      'user-agent': 'covenant-x402-seller',
      'x-github-api-version': '2022-11-28',
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (response.status === 404) {
    throw new MaintenanceLookupError(404, 'repository not found or not public');
  }
  if (response.status === 403 || response.status === 429) {
    throw new MaintenanceLookupError(503, 'GitHub rate limit reached, try again shortly');
  }
  if (!response.ok) throw new MaintenanceLookupError(502, 'GitHub is unavailable');
  return response.json();
}

export async function assessRepository(
  owner: string,
  repo: string,
  options: { token?: string; timeoutMs?: number; now?: () => Date } = {},
): Promise<MaintenanceAssessment> {
  const timeoutMs = options.timeoutMs ?? 9_000;
  const observedAt = (options.now?.() ?? new Date()).toISOString();
  const repository = `${owner}/${repo}`;
  const base = { repository, observedAt, supportedManifests: SUPPORTED_MANIFESTS };

  const metadata = (await githubJson(
    `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`,
    options.token,
    timeoutMs,
  )) as { private?: boolean; default_branch?: string; archived?: boolean };

  if (metadata.private) {
    return { ...base, eligible: false, reason: 'Mizuki maintains public repositories only.' };
  }
  if (metadata.archived) {
    return { ...base, eligible: false, reason: 'This repository is archived.' };
  }

  const contents = (await githubJson(
    `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/contents/`,
    options.token,
    timeoutMs,
  )) as Array<{ name?: unknown; type?: unknown }>;

  if (!Array.isArray(contents)) throw new MaintenanceLookupError(502, 'GitHub is unavailable');
  const rootFiles = contents
    .slice(0, MAX_ROOT_ENTRIES)
    .filter((entry) => entry?.type === 'file' && typeof entry.name === 'string')
    .map((entry) => entry.name as string);

  const match = validationCommandFor(rootFiles);
  if (!match) {
    return {
      ...base,
      eligible: false,
      defaultBranch: metadata.default_branch,
      reason: `No supported validation command. Mizuki runs the command implied by a manifest in the repository root: ${SUPPORTED_MANIFESTS.join(', ')}.`,
    };
  }

  return {
    ...base,
    eligible: true,
    defaultBranch: metadata.default_branch,
    detectedManifest: match[0],
    validationCommand: match[1],
  };
}
