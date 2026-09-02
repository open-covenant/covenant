/**
 * Mizuki tools for JavaScript agents.
 *
 * Mizuki takes one authorized issue from a public GitHub repository and returns a
 * pull request that passes that repository's own checks, or refunds the quoted
 * amount. Payment is an exact USDC transfer on Solana mainnet with a sponsored
 * fee payer, so a caller needs USDC but not SOL.
 *
 * Every method returns a string, because that is what an agent framework hands
 * back to a model. A refusal is returned as text rather than thrown: the reason
 * a repository was rejected is something the model should read and relay, not an
 * exception to retry blindly.
 */

export const MIZUKI_API_URL = 'https://mizuki.opencovenant.org/api/mizuki';

const REQUEST_TIMEOUT_MS = 20_000;

/** GitHub owner and repository names. Dots are legal, so `.` and `..` are excluded separately. */
const GITHUB_NAME = /^[A-Za-z0-9._-]{1,100}$/;

export const isGithubName = (value: string): boolean =>
  GITHUB_NAME.test(value) && value !== '.' && value !== '..';

export interface MizukiToolsetOptions {
  /** Mizuki API base URL. Defaults to the public service. */
  apiUrl?: string;
  /** Maintainer token. Only the repository and preflight reads need one. */
  apiToken?: string;
  /** Per-request timeout in milliseconds. */
  timeoutMs?: number;
  /** Injected for tests. */
  fetchImpl?: typeof fetch;
}

export class MizukiToolset {
  private readonly apiUrl: string;
  private readonly apiToken?: string;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;

  constructor(options: MizukiToolsetOptions = {}) {
    this.apiUrl = (options.apiUrl ?? process.env.MIZUKI_API_URL ?? MIZUKI_API_URL).replace(
      /\/$/,
      '',
    );
    this.apiToken = options.apiToken ?? process.env.MIZUKI_API_TOKEN;
    this.timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
    this.fetchImpl = options.fetchImpl ?? fetch;
  }

  private async request(
    path: string,
    init: RequestInit = {},
  ): Promise<{ status: number; body: unknown; challenge?: unknown }> {
    const response = await this.fetchImpl(`${this.apiUrl}${path}`, {
      ...init,
      headers: {
        accept: 'application/json',
        ...(this.apiToken ? { authorization: `Bearer ${this.apiToken}` } : {}),
        ...(init.headers ?? {}),
      },
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    const text = await response.text();
    let body: unknown = text;
    try {
      body = text ? JSON.parse(text) : {};
    } catch {
      // A non-JSON body from an upstream is a failure to answer, not a verdict.
    }
    // A 402 carries its challenge in the header, and the body is empty. Returning
    // only the body would tell an agent to pay for something it cannot see.
    let challenge: unknown;
    const header = response.headers.get('payment-required');
    if (header) {
      try {
        challenge = JSON.parse(Buffer.from(header, 'base64').toString('utf8'));
      } catch {
        challenge = header;
      }
    }
    return { status: response.status, body, ...(challenge ? { challenge } : {}) };
  }

  /** Quote fixed-price maintenance for one public GitHub issue. */
  async quote(githubIssueUrl: string): Promise<string> {
    try {
      const { status, body } = await this.request('/v1/quotes', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: githubIssueUrl }),
      });
      if (status >= 400) return `Mizuki declined to quote this issue: ${JSON.stringify(body)}`;
      return JSON.stringify(body, null, 2);
    } catch (error) {
      return `Could not reach Mizuki to quote this issue: ${String(error)}`;
    }
  }

  /** Report whether a repository qualifies, and the command Mizuki would run to validate a change. */
  async assess(owner: string, repo: string): Promise<string> {
    if (!isGithubName(owner) || !isGithubName(repo)) {
      return 'The owner and repo must be GitHub names.';
    }
    try {
      const { status, body, challenge } = await this.request(
        `/x402/assess/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`,
      );
      if (status === 402) {
        return `This assessment is a paid endpoint. Pay this x402 challenge to read it:\n${JSON.stringify(challenge ?? body, null, 2)}`;
      }
      if (status >= 400) return `Could not assess that repository: ${JSON.stringify(body)}`;
      return JSON.stringify(body, null, 2);
    } catch (error) {
      return `Could not reach Mizuki to assess that repository: ${String(error)}`;
    }
  }

  /** Read delivery, pull request, validation, and refund state for a job. */
  async jobStatus(jobId: string): Promise<string> {
    try {
      const { status, body } = await this.request(`/v1/jobs/${encodeURIComponent(jobId)}`);
      if (status === 404) return `No Mizuki job found with id ${jobId}.`;
      if (status >= 400) return `Could not read that Mizuki job: ${JSON.stringify(body)}`;
      return JSON.stringify(body, null, 2);
    } catch (error) {
      return `Could not reach Mizuki to read that job: ${String(error)}`;
    }
  }

  /** List open public maintenance bounties. */
  async bounties(): Promise<string> {
    try {
      const { status, body } = await this.request('/v1/bounties');
      if (status >= 400) return `Could not list Mizuki bounties: ${JSON.stringify(body)}`;
      return JSON.stringify(body, null, 2);
    } catch (error) {
      return `Could not reach Mizuki to list bounties: ${String(error)}`;
    }
  }
}
