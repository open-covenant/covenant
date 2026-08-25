import { createHmac, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { address, getPublicKeyFromAddress } from '@solana/kit';
import { z } from 'zod';
import type { Config } from './config.js';
import type { GithubIdentityRegistrar } from './policy-client.js';
import { StateConflictError, type MizukiStore } from './store.js';
import type { Contributor, WalletChallenge } from './types.js';

const githubUserSchema = z.object({ id: z.number().int().positive(), login: z.string().min(1) });
const tokenSchema = z.object({
  githubId: z.string(),
  githubLogin: z.string(),
  githubGrantId: z.string().uuid().optional(),
  githubGrantExpiresAt: z.string().datetime({ offset: true }).optional(),
  exp: z.number().int(),
});
const stateSchema = z.object({
  redirect: z.string(),
  exp: z.number().int(),
  nonce: z.string().uuid(),
  browserBinding: z.string().regex(/^[A-Za-z0-9_-]{43}$/),
});

export const GITHUB_OAUTH_FLOW_TTL_SECONDS = 10 * 60;

export type GithubOAuthErrorCode =
  | 'denied'
  | 'expired'
  | 'incomplete'
  | 'invalid'
  | 'replayed'
  | 'unavailable';

export class GithubOAuthCallbackError extends Error {
  constructor(readonly code: Extract<GithubOAuthErrorCode, 'expired' | 'invalid' | 'replayed'>) {
    super(
      code === 'expired'
        ? 'OAuth browser flow expired'
        : code === 'replayed'
          ? 'OAuth browser flow was already used'
          : 'OAuth browser flow is invalid',
    );
  }
}

export type ContributorSession = z.infer<typeof tokenSchema>;

export class ContributorAuth {
  constructor(
    private readonly config: Pick<
      Config,
      'publicBaseUrl' | 'webOrigin' | 'githubClientId' | 'githubClientSecret' | 'sessionSecret'
    >,
    private readonly store: MizukiStore,
    private readonly request: typeof fetch = fetch,
    private readonly identityRegistrar?: GithubIdentityRegistrar,
  ) {}

  configured(): boolean {
    return Boolean(
      this.config.githubClientId &&
      this.config.githubClientSecret &&
      this.config.sessionSecret &&
      this.config.webOrigin,
    );
  }

  async beginGithubOAuth(redirect = '/bounties'): Promise<{ url: string; flowCookie: string }> {
    this.assertConfigured();
    const safeRedirect = githubOAuthRedirectPath(redirect);
    const now = Date.now();
    const expiresAt = now + GITHUB_OAUTH_FLOW_TTL_SECONDS * 1_000;
    const nonce = randomUUID();
    const flowCookie = randomBytes(32).toString('base64url');
    const browserBinding = this.browserBinding(flowCookie);
    await this.store.saveGithubOAuthFlow({
      id: nonce,
      binding: browserBinding,
      expiresAt: new Date(expiresAt).toISOString(),
      createdAt: new Date(now).toISOString(),
    });
    const state = this.sign({
      redirect: safeRedirect,
      exp: expiresAt,
      nonce,
      browserBinding,
    });
    const callback = `${this.config.webOrigin!.replace(/\/$/, '')}/api/mizuki/v1/auth/github/callback`;
    const query = new URLSearchParams({
      client_id: this.config.githubClientId!,
      redirect_uri: callback,
      scope: 'read:user',
      state,
    });
    return { url: `https://github.com/login/oauth/authorize?${query}`, flowCookie };
  }

  async callback(
    code: string,
    signedState: string,
    flowCookie: string | undefined,
  ): Promise<{
    contributor: Contributor;
    session: string;
    redirect: string;
  }> {
    this.assertConfigured();
    let state: z.infer<typeof stateSchema>;
    try {
      state = stateSchema.parse(this.verify(signedState));
    } catch {
      throw new GithubOAuthCallbackError('invalid');
    }
    if (state.exp <= Date.now()) throw new GithubOAuthCallbackError('expired');
    if (!flowCookie) throw new GithubOAuthCallbackError('invalid');
    const browserBinding = this.browserBinding(flowCookie);
    if (!equal(browserBinding, state.browserBinding)) {
      throw new GithubOAuthCallbackError('invalid');
    }
    try {
      await this.store.consumeGithubOAuthFlow(state.nonce, state.browserBinding);
    } catch (cause) {
      if (cause instanceof StateConflictError) {
        throw new GithubOAuthCallbackError(
          /already used/i.test(cause.message) ? 'replayed' : 'expired',
        );
      }
      if (cause instanceof Error && cause.message === 'OAuth browser flow is invalid') {
        throw new GithubOAuthCallbackError('invalid');
      }
      throw cause;
    }
    const tokenResponse = await this.request('https://github.com/login/oauth/access_token', {
      method: 'POST',
      headers: { accept: 'application/json', 'content-type': 'application/json' },
      body: JSON.stringify({
        client_id: this.config.githubClientId,
        client_secret: this.config.githubClientSecret,
        code,
      }),
      signal: AbortSignal.timeout(15_000),
    });
    if (!tokenResponse.ok) throw new Error(`GitHub OAuth exchange failed: ${tokenResponse.status}`);
    const tokenBody = z
      .object({ access_token: z.string().min(1) })
      .parse(await tokenResponse.json());
    const userResponse = await this.request('https://api.github.com/user', {
      headers: {
        accept: 'application/vnd.github+json',
        authorization: `Bearer ${tokenBody.access_token}`,
        'user-agent': 'mizuki-maintainer',
        'x-github-api-version': '2022-11-28',
      },
      signal: AbortSignal.timeout(15_000),
    });
    if (!userResponse.ok) throw new Error(`GitHub user lookup failed: ${userResponse.status}`);
    const user = githubUserSchema.parse(await userResponse.json());
    const grant = this.identityRegistrar
      ? await this.identityRegistrar.registerGithubIdentity(tokenBody.access_token)
      : undefined;
    if (
      grant &&
      (grant.githubId !== String(user.id) || grant.login.toLowerCase() !== user.login.toLowerCase())
    ) {
      throw new Error('policy signer GitHub identity does not match the OAuth user');
    }
    const contributor = await this.store.upsertContributor(String(user.id), user.login);
    const session = this.sign({
      githubId: contributor.githubId,
      githubLogin: contributor.githubLogin,
      ...(grant ? { githubGrantId: grant.id, githubGrantExpiresAt: grant.expiresAt } : {}),
      exp: Date.now() + 7 * 24 * 60 * 60_000,
    });
    return { contributor, session, redirect: state.redirect };
  }

  session(value: string | undefined): ContributorSession | undefined {
    if (!value || !this.config.sessionSecret) return undefined;
    try {
      const session = tokenSchema.parse(this.verify(value));
      return session.exp > Date.now() ? session : undefined;
    } catch {
      return undefined;
    }
  }

  async createWalletChallenge(
    session: ContributorSession,
    walletValue: string,
  ): Promise<WalletChallenge> {
    const wallet = address(walletValue);
    const now = new Date();
    const expiresAt = new Date(now.getTime() + 5 * 60_000);
    const id = randomUUID();
    const message = [
      'Mizuki contributor wallet verification',
      `URI: ${this.config.webOrigin}`,
      `GitHub: ${session.githubLogin} (${session.githubId})`,
      `Wallet: ${wallet}`,
      `Nonce: ${id}`,
      `Issued At: ${now.toISOString()}`,
      `Expiration Time: ${expiresAt.toISOString()}`,
      'Purpose: receive contributor escrow payments',
    ].join('\n');
    return this.store.saveWalletChallenge({
      id,
      githubId: session.githubId,
      wallet,
      message,
      expiresAt: expiresAt.toISOString(),
      createdAt: now.toISOString(),
    });
  }

  async verifyWalletChallenge(
    session: ContributorSession,
    challengeId: string,
    signatureBase64: string,
  ): Promise<Contributor> {
    const challenge = await this.store.consumeWalletChallenge(challengeId, session.githubId);
    const signature = Buffer.from(signatureBase64, 'base64');
    if (signature.length !== 64) throw new Error('wallet signature must be 64 bytes');
    const key = await getPublicKeyFromAddress(address(challenge.wallet));
    const valid = await globalThis.crypto.subtle.verify(
      'Ed25519',
      key,
      signature,
      new TextEncoder().encode(challenge.message),
    );
    if (!valid) throw new Error('wallet signature is invalid');
    return this.store.linkContributorWallet(session.githubId, challenge.wallet);
  }

  private sign(payload: Record<string, unknown>): string {
    if (!this.config.sessionSecret) throw new Error('MIZUKI_SESSION_SECRET is not configured');
    const encoded = Buffer.from(JSON.stringify(payload)).toString('base64url');
    return `${encoded}.${mac(encoded, this.config.sessionSecret)}`;
  }

  private verify(value: string): unknown {
    if (!this.config.sessionSecret) throw new Error('MIZUKI_SESSION_SECRET is not configured');
    const [payload, signature, ...rest] = value.split('.');
    if (!payload || !signature || rest.length > 0) throw new Error('invalid signed value');
    const expected = mac(payload, this.config.sessionSecret);
    if (
      signature.length !== expected.length ||
      !timingSafeEqual(Buffer.from(signature), Buffer.from(expected))
    ) {
      throw new Error('invalid signed value');
    }
    return JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as unknown;
  }

  private browserBinding(flowCookie: string): string {
    if (!this.config.sessionSecret) throw new Error('MIZUKI_SESSION_SECRET is not configured');
    return createHmac('sha256', this.config.sessionSecret)
      .update('mizuki.github-oauth-flow.v1\0')
      .update(flowCookie)
      .digest('base64url');
  }

  private assertConfigured(): void {
    if (!this.config.githubClientId || !this.config.githubClientSecret) {
      throw new Error('GitHub OAuth is not configured');
    }
    if (!this.config.webOrigin || !this.config.sessionSecret) {
      throw new Error('contributor session configuration is incomplete');
    }
  }
}

export function githubOAuthRedirectPath(value: string | undefined, fallback = '/bounties'): string {
  if (!value) return fallback;
  try {
    const base = new URL('https://mizuki.invalid');
    const target = new URL(value, base);
    if (target.origin !== base.origin || target.hash) return fallback;
    const allowed =
      target.pathname === '/app' ||
      target.pathname.startsWith('/app/') ||
      target.pathname === '/bounties' ||
      target.pathname.startsWith('/bounties/');
    return allowed ? `${target.pathname}${target.search}` : fallback;
  } catch {
    return fallback;
  }
}

function mac(value: string, secret: string): string {
  return createHmac('sha256', secret).update(value).digest('base64url');
}

function equal(left: string, right: string): boolean {
  return left.length === right.length && timingSafeEqual(Buffer.from(left), Buffer.from(right));
}
