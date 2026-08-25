type RequestOptions = {
  method?: 'GET' | 'POST';
  body?: unknown;
  headers?: Record<string, string>;
  authenticated?: boolean;
};

export type MizukiMcpClientOptions = {
  baseUrl: string;
  apiToken?: string;
  timeoutMs?: number;
  request?: typeof fetch;
};

export class MizukiMcpClient {
  private readonly baseUrl: string;
  private readonly apiToken?: string;
  private readonly timeoutMs: number;
  private readonly requestFn: typeof fetch;

  constructor(options: MizukiMcpClientOptions) {
    this.baseUrl = validatedBaseUrl(options.baseUrl);
    this.apiToken = options.apiToken;
    this.timeoutMs = boundedTimeout(options.timeoutMs);
    this.requestFn = options.request ?? fetch;
  }

  async call(path: string, options: RequestOptions = {}): Promise<unknown> {
    if (!path.startsWith('/v1/')) throw new Error('Mizuki MCP API path must start with /v1/');
    const headers = new Headers(options.headers);
    headers.delete('authorization');
    headers.delete('cookie');
    headers.set('accept', 'application/json');
    if (options.body !== undefined) headers.set('content-type', 'application/json');
    if (options.authenticated) {
      if (!this.apiToken) {
        throw new Error('This MCP tool requires a scoped Mizuki API token in MIZUKI_API_TOKEN');
      }
      headers.set('authorization', `Bearer ${this.apiToken}`);
    }
    const signal = AbortSignal.timeout(this.timeoutMs);
    let response: Response;
    try {
      response = await this.requestFn(`${this.baseUrl}${path}`, {
        method: options.method ?? (options.body === undefined ? 'GET' : 'POST'),
        headers,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal,
      });
    } catch (cause) {
      if (signal.aborted) {
        throw new Error(`Mizuki API request timed out after ${this.timeoutMs}ms`);
      }
      throw cause;
    }
    let value: unknown;
    try {
      value = await response.json();
    } catch (cause) {
      if (signal.aborted) {
        throw new Error(`Mizuki API request timed out after ${this.timeoutMs}ms`);
      }
      if (cause instanceof SyntaxError) {
        throw new Error(`Mizuki API ${response.status}: invalid JSON response`);
      }
      throw cause;
    }
    if (!response.ok) throw new Error(`Mizuki API ${response.status}: ${JSON.stringify(value)}`);
    return value;
  }

  async repositories(): Promise<unknown> {
    return this.call('/v1/account/repositories', { authenticated: true });
  }

  async quote(issueUrl: string): Promise<unknown> {
    return this.call(this.apiToken ? '/v1/account/quotes' : '/v1/quotes', {
      authenticated: Boolean(this.apiToken),
      method: 'POST',
      body: { github_issue_url: issueUrl },
    });
  }

  async repositoryReadiness(owner: string, repo: string): Promise<unknown> {
    const value = await this.repositories();
    const repositories = record(value).repositories;
    const match = Array.isArray(repositories)
      ? repositories.find((candidate) => {
          const repository = record(candidate);
          return (
            text(repository.owner)?.toLowerCase() === owner.toLowerCase() &&
            text(repository.repo)?.toLowerCase() === repo.toLowerCase()
          );
        })
      : undefined;
    return match
      ? { repository: match }
      : {
          repository: `${owner}/${repo}`,
          status: 'not_connected',
          action:
            'Connect and verify this repository in Mizuki Workbench before requesting issue data.',
        };
  }

  async issues(owner: string, repo: string): Promise<unknown> {
    return this.call(
      `/v1/repositories/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/issues`,
      { authenticated: true },
    );
  }

  async preflight(issueUrl: string): Promise<unknown> {
    return this.call('/v1/preflights', {
      authenticated: true,
      method: 'POST',
      body: { github_issue_url: issueUrl },
    });
  }

  async paymentStatus(quoteId: string, idempotencyKey: string): Promise<unknown> {
    return this.call(`/v1/account/quotes/${encodeURIComponent(quoteId)}/payment-status`, {
      authenticated: true,
      headers: { 'idempotency-key': idempotencyKey },
    });
  }
}

function validatedBaseUrl(value: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error('Mizuki MCP API URL is invalid');
  }
  const loopback =
    url.hostname === 'localhost' || url.hostname === '127.0.0.1' || url.hostname === '[::1]';
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) {
    throw new Error('Mizuki MCP API URL must use HTTPS outside local development');
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new Error('Mizuki MCP API URL cannot include credentials, a query, or a fragment');
  }
  return url.toString().replace(/\/$/, '');
}

function boundedTimeout(value: number | undefined): number {
  if (value === undefined) return 10_000;
  if (!Number.isInteger(value) || value < 1_000 || value > 60_000) {
    throw new Error('Mizuki MCP timeout must be between 1000 and 60000 milliseconds');
  }
  return value;
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {};
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}
