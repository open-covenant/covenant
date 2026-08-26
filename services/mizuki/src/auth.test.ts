import { createHmac, generateKeyPairSync, sign } from 'node:crypto';
import { getBase58Decoder } from '@solana/kit';
import { describe, expect, it, vi } from 'vitest';
import { createApiToken } from './api-tokens.js';
import { ApiTokenAuthError, ContributorAuth, GithubOAuthCallbackError } from './auth.js';
import { MemoryStore } from './store.js';

describe('ContributorAuth', () => {
  it('binds CSRF tokens to a valid browser session', () => {
    const secret = 's'.repeat(32);
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: secret,
      },
      new MemoryStore(),
    );
    const session = signedSession(
      { githubId: '42', githubLogin: 'maintainer', exp: Date.now() + 60_000 },
      secret,
    );
    const otherSession = signedSession(
      { githubId: '7', githubLogin: 'contributor', exp: Date.now() + 60_000 },
      secret,
    );
    const token = auth.csrfToken(session);

    expect(token).toMatch(/^[A-Za-z0-9_-]{43}$/);
    expect(auth.verifyCsrfToken(session, token)).toBe(true);
    expect(auth.verifyCsrfToken(otherSession, token)).toBe(false);
    expect(auth.verifyCsrfToken(session, 'invalid')).toBe(false);
    expect(auth.csrfToken('invalid-session')).toBeUndefined();
  });

  it('authenticates scoped API tokens, records use, and fails closed after revocation', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: 's'.repeat(32),
      },
      store,
    );
    const credential = createApiToken({
      githubId: '42',
      name: 'MCP',
      scopes: ['repositories:read'],
      expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
    });
    await store.createApiToken(credential.record);

    await expect(auth.apiToken(credential.token, 'repositories:read')).resolves.toMatchObject({
      kind: 'api_token',
      tokenId: credential.record.id,
      githubId: '42',
      githubLogin: 'maintainer',
      scopes: ['repositories:read'],
    });
    expect((await store.apiTokenByPrefix(credential.record.prefix))?.lastUsedAt).toBeTruthy();
    await expect(
      auth.apiToken(credential.token, 'jobs:write'),
    ).rejects.toMatchObject<ApiTokenAuthError>({ code: 'insufficient_scope' });

    const wrongToken = `${credential.token.slice(0, -1)}${credential.token.endsWith('a') ? 'b' : 'a'}`;
    await expect(
      auth.apiToken(wrongToken, 'repositories:read'),
    ).rejects.toMatchObject<ApiTokenAuthError>({ code: 'invalid' });
    await store.revokeApiToken(credential.record.id, '42', new Date().toISOString());
    await expect(
      auth.apiToken(credential.token, 'repositories:read'),
    ).rejects.toMatchObject<ApiTokenAuthError>({ code: 'invalid' });

    const expired = createApiToken({
      githubId: '42',
      name: 'Expired MCP',
      scopes: ['repositories:read'],
      expiresAt: new Date(Date.now() + 24 * 60 * 60_000).toISOString(),
    });
    expired.record.createdAt = '2026-01-01T00:00:00.000Z';
    expired.record.expiresAt = '2026-01-02T00:00:00.000Z';
    await store.createApiToken(expired.record);
    await expect(
      auth.apiToken(expired.token, 'repositories:read'),
    ).rejects.toMatchObject<ApiTokenAuthError>({ code: 'invalid' });
  });

  it('routes the OAuth callback through the web origin so the session cookie is first-party', async () => {
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: 's'.repeat(32),
      },
      new MemoryStore(),
    );
    const authorization = await auth.beginGithubOAuth('/bounties/bounty-1');
    expect(new URL(authorization.url).searchParams.get('prompt')).toBe('select_account');
    const authorize = new URL(authorization.url);
    expect(authorize.searchParams.get('redirect_uri')).toBe(
      'https://mizuki.example/api/mizuki/v1/auth/github/callback',
    );
  });

  it('keeps redirects on the first-party path allowlist', async () => {
    const auth = oauthAuth(new MemoryStore(), vi.fn());
    for (const destination of [
      'https://attacker.example/session',
      '//attacker.example/app',
      '/application',
      '/settings',
      '/app#credential',
    ]) {
      const authorization = await auth.beginGithubOAuth(destination);
      const payload = signedPayload(new URL(authorization.url).searchParams.get('state')!);
      expect(payload).toMatchObject({ redirect: '/bounties' });
    }

    const allowed = await auth.beginGithubOAuth('/app/jobs/new?issue=7');
    expect(signedPayload(new URL(allowed.url).searchParams.get('state')!)).toMatchObject({
      redirect: '/app/jobs/new?issue=7',
    });
  });

  it('recovers a verified return path from denied, expired, or replayed callback state', async () => {
    const now = Date.parse('2026-08-25T12:00:00.000Z');
    const clock = vi.spyOn(Date, 'now').mockReturnValue(now);
    const auth = oauthAuth(new MemoryStore(), vi.fn());
    const publicFlow = await auth.beginGithubOAuth('/bounties/bounty-1?view=criteria');
    const workbenchFlow = await auth.beginGithubOAuth('/app/jobs/new?issue=7&repository=tool');
    const publicState = new URL(publicFlow.url).searchParams.get('state')!;
    const workbenchState = new URL(workbenchFlow.url).searchParams.get('state')!;

    expect(auth.githubOAuthRedirect(publicState)).toBe('/bounties/bounty-1?view=criteria');
    expect(auth.githubOAuthRedirect(workbenchState)).toBe('/app/jobs/new?issue=7&repository=tool');
    clock.mockReturnValue(now + 10 * 60_000 + 1);
    expect(auth.githubOAuthRedirect(publicState)).toBe('/bounties/bounty-1?view=criteria');
    expect(auth.githubOAuthRedirect(`${publicState}x`)).toBeUndefined();
    expect(auth.githubOAuthRedirect(undefined)).toBeUndefined();
    clock.mockRestore();
  });

  it('requires the browser flow cookie and rejects a different browser before token exchange', async () => {
    const request = oauthRequests();
    const auth = oauthAuth(new MemoryStore(), request);
    const authorization = await auth.beginGithubOAuth('/app');
    const state = new URL(authorization.url).searchParams.get('state')!;

    await expect(auth.callback('code', state, undefined)).rejects.toMatchObject({
      code: 'invalid',
    });
    await expect(auth.callback('code', state, 'different-browser')).rejects.toMatchObject({
      code: 'invalid',
    });
    expect(request).not.toHaveBeenCalled();
    await expect(auth.callback('code', state, authorization.flowCookie)).resolves.toMatchObject({
      redirect: '/app',
    });
  });

  it('consumes the browser flow before exchanging the code and rejects replay', async () => {
    const request = oauthRequests();
    const auth = oauthAuth(new MemoryStore(), request);
    const authorization = await auth.beginGithubOAuth('/app');
    const state = new URL(authorization.url).searchParams.get('state')!;

    await expect(auth.callback('code', state, authorization.flowCookie)).resolves.toMatchObject({
      redirect: '/app',
    });
    await expect(
      auth.callback('code', state, authorization.flowCookie),
    ).rejects.toMatchObject<GithubOAuthCallbackError>({ code: 'replayed' });
    expect(request).toHaveBeenCalledTimes(2);
  });

  it('rejects an expired browser flow before token exchange', async () => {
    const now = Date.parse('2026-08-25T12:00:00.000Z');
    const clock = vi.spyOn(Date, 'now').mockReturnValue(now);
    const request = vi.fn();
    const auth = oauthAuth(new MemoryStore(), request);
    const authorization = await auth.beginGithubOAuth('/app');
    const state = new URL(authorization.url).searchParams.get('state')!;
    clock.mockReturnValue(now + 10 * 60_000 + 1);

    await expect(
      auth.callback('code', state, authorization.flowCookie),
    ).rejects.toMatchObject<GithubOAuthCallbackError>({ code: 'expired' });
    expect(request).not.toHaveBeenCalled();
    clock.mockRestore();
  });

  it('preserves OAuth store outages for the callback availability response', async () => {
    const store = new MemoryStore();
    const auth = oauthAuth(store, vi.fn());
    const authorization = await auth.beginGithubOAuth('/app');
    const state = new URL(authorization.url).searchParams.get('state')!;
    vi.spyOn(store, 'consumeGithubOAuthFlow').mockRejectedValueOnce(
      new Error('database temporarily unavailable'),
    );

    await expect(auth.callback('code', state, authorization.flowCookie)).rejects.toThrow(
      'database temporarily unavailable',
    );
  });

  it('authorizes a connected pull request as the signed-in maintainer without storing OAuth access', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'open-covenant', 'covenant');
    const requests = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.includes('/login/oauth/access_token')) {
        return Response.json({ access_token: 'temporary-token', scope: 'read:user,public_repo' });
      }
      if (url === 'https://api.github.com/user') {
        return Response.json({ id: 42, login: 'maintainer' });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant') {
        return Response.json({
          private: false,
          permissions: { admin: false, maintain: true, push: true, triage: true },
        });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant/pulls/196') {
        return Response.json({ state: 'open' });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant/issues/196/labels') {
        expect(init?.method).toBe('POST');
        expect(JSON.parse(String(init?.body))).toEqual({ labels: ['mizuki:authorized'] });
        return Response.json([{ name: 'mizuki:authorized' }]);
      }
      throw new Error(`unexpected request: ${url}`);
    });
    const auth = oauthAuth(store, requests);
    const authorization = await auth.beginGithubOAuth(
      '/app/jobs/new?owner=open-covenant&repo=covenant',
      'https://github.com/open-covenant/covenant/pull/196',
    );
    const authorizeUrl = new URL(authorization.url);
    expect(authorizeUrl.searchParams.get('scope')).toBe('read:user public_repo');

    await expect(
      auth.callback('code', authorizeUrl.searchParams.get('state')!, authorization.flowCookie),
    ).resolves.toMatchObject({
      redirect: '/app/jobs/new?owner=open-covenant&repo=covenant',
    });
    expect(JSON.stringify(await store.contributor('42'))).not.toContain('temporary-token');
  });

  it('reports a merged pull request as inactive without attempting to label it', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'open-covenant', 'covenant');
    const requests = vi.fn(async (input: string | URL | Request) => {
      const url = String(input);
      if (url.includes('/login/oauth/access_token')) {
        return Response.json({ access_token: 'temporary-token', scope: 'read:user,public_repo' });
      }
      if (url === 'https://api.github.com/user') {
        return Response.json({ id: 42, login: 'maintainer' });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant') {
        return Response.json({
          private: false,
          permissions: { admin: false, maintain: true, push: true, triage: true },
        });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant/pulls/169') {
        return Response.json({ state: 'closed', merged_at: '2026-08-23T12:00:00.000Z' });
      }
      throw new Error(`unexpected request: ${url}`);
    });
    const auth = oauthAuth(store, requests);
    const authorization = await auth.beginGithubOAuth(
      '/app/jobs/new?owner=open-covenant&repo=covenant',
      'https://github.com/open-covenant/covenant/pull/169',
    );

    await expect(
      auth.callback(
        'code',
        new URL(authorization.url).searchParams.get('state')!,
        authorization.flowCookie,
      ),
    ).rejects.toMatchObject<GithubOAuthCallbackError>({ code: 'inactive' });
    expect(requests.mock.calls.map(([input]) => String(input))).not.toContain(
      'https://api.github.com/repos/open-covenant/covenant/issues/169/labels',
    );
  });

  it('authorizes a connected issue as the signed-in maintainer without storing OAuth access', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'open-covenant', 'covenant');
    const requests = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.includes('/login/oauth/access_token')) {
        return Response.json({ access_token: 'temporary-token', scope: 'read:user,public_repo' });
      }
      if (url === 'https://api.github.com/user') {
        return Response.json({ id: 42, login: 'maintainer' });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant') {
        return Response.json({
          private: false,
          permissions: { admin: false, maintain: true, push: true, triage: true },
        });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant/issues/197') {
        return Response.json({ state: 'open' });
      }
      if (url === 'https://api.github.com/repos/open-covenant/covenant/issues/197/labels') {
        expect(init?.method).toBe('POST');
        expect(JSON.parse(String(init?.body))).toEqual({ labels: ['mizuki:authorized'] });
        return Response.json([{ name: 'mizuki:authorized' }]);
      }
      throw new Error(`unexpected request: ${url}`);
    });
    const auth = oauthAuth(store, requests);
    const authorization = await auth.beginGithubOAuth(
      '/app/repositories/open-covenant/covenant',
      undefined,
      'https://github.com/open-covenant/covenant/issues/197',
    );
    const authorizeUrl = new URL(authorization.url);
    expect(authorizeUrl.searchParams.get('scope')).toBe('read:user public_repo');

    await expect(
      auth.callback('code', authorizeUrl.searchParams.get('state')!, authorization.flowCookie),
    ).resolves.toMatchObject({ redirect: '/app/repositories/open-covenant/covenant' });
    expect(JSON.stringify(await store.contributor('42'))).not.toContain('temporary-token');
  });

  it('rejects pull request URLs submitted as issue authorization targets', async () => {
    const auth = oauthAuth(new MemoryStore(), vi.fn());

    await expect(
      auth.beginGithubOAuth(
        '/app',
        undefined,
        'https://github.com/open-covenant/covenant/pull/196',
      ),
    ).rejects.toMatchObject<GithubOAuthCallbackError>({ code: 'invalid' });
  });

  it('fails closed when GitHub does not grant pull request authorization scope', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'open-covenant', 'covenant');
    const auth = oauthAuth(store, oauthRequests());
    const authorization = await auth.beginGithubOAuth(
      '/app/jobs/new',
      'https://github.com/open-covenant/covenant/pull/196',
    );

    await expect(
      auth.callback(
        'code',
        new URL(authorization.url).searchParams.get('state')!,
        authorization.flowCookie,
      ),
    ).rejects.toMatchObject<GithubOAuthCallbackError>({ code: 'permission' });
  });

  it('links a wallet after a valid domain-bound signature and rejects replay', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: 's'.repeat(32),
      },
      store,
    );
    const { publicKey, privateKey } = generateKeyPairSync('ed25519');
    const der = publicKey.export({ format: 'der', type: 'spki' });
    const wallet = getBase58Decoder().decode(der.subarray(der.length - 32));
    const session = { githubId: '42', githubLogin: 'maintainer', exp: Date.now() + 60_000 };
    const challenge = await auth.createWalletChallenge(session, wallet);
    const signature = sign(null, Buffer.from(challenge.message), privateKey).toString('base64');

    await expect(
      auth.verifyWalletChallenge(session, challenge.id, signature),
    ).resolves.toMatchObject({ wallet });
    await expect(auth.verifyWalletChallenge(session, challenge.id, signature)).rejects.toThrow(
      'already consumed',
    );
  });

  it('does not link a wallet when the signature is invalid', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('7', 'contributor');
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: 's'.repeat(32),
      },
      store,
    );
    const { publicKey } = generateKeyPairSync('ed25519');
    const der = publicKey.export({ format: 'der', type: 'spki' });
    const wallet = getBase58Decoder().decode(der.subarray(der.length - 32));
    const session = { githubId: '7', githubLogin: 'contributor', exp: Date.now() + 60_000 };
    const challenge = await auth.createWalletChallenge(session, wallet);

    await expect(
      auth.verifyWalletChallenge(session, challenge.id, Buffer.alloc(64).toString('base64')),
    ).rejects.toThrow('invalid');
    expect((await store.contributor('7'))?.wallet).toBeUndefined();
  });

  it('exchanges the OAuth token for a signer-issued identity grant without persisting it', async () => {
    const store = new MemoryStore();
    const accessToken = 'temporary-oauth-token';
    const requests = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.includes('/login/oauth/access_token')) {
        return Response.json({ access_token: accessToken });
      }
      if (url === 'https://api.github.com/user') {
        return Response.json({ id: 42, login: 'maintainer' });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    let registeredToken: string | undefined;
    const registrar = {
      registerGithubIdentity: async (token: string) => {
        registeredToken = token;
        return {
          id: '10000000-0000-4000-8000-000000000001',
          githubId: '42',
          login: 'maintainer',
          expiresAt: new Date(Date.now() + 10 * 60_000).toISOString(),
        };
      },
    };
    const auth = new ContributorAuth(
      {
        publicBaseUrl: 'https://api.mizuki.example',
        webOrigin: 'https://mizuki.example',
        githubClientId: 'client',
        githubClientSecret: 'secret',
        sessionSecret: 's'.repeat(32),
      },
      store,
      requests as typeof fetch,
      registrar,
    );
    const authorization = await auth.beginGithubOAuth();
    const state = new URL(authorization.url).searchParams.get('state')!;
    const result = await auth.callback('code', state, authorization.flowCookie);
    expect(registeredToken).toBe(accessToken);
    expect(auth.session(result.session)).toMatchObject({
      githubId: '42',
      githubGrantId: '10000000-0000-4000-8000-000000000001',
    });
    expect(JSON.stringify(await store.contributor('42'))).not.toContain(accessToken);
    expect(result.session).not.toContain(accessToken);
  });
});

function oauthAuth(store: MemoryStore, request: typeof fetch | ReturnType<typeof vi.fn>) {
  return new ContributorAuth(
    {
      publicBaseUrl: 'https://api.mizuki.example',
      webOrigin: 'https://mizuki.example',
      githubClientId: 'client',
      githubClientSecret: 'secret',
      sessionSecret: 's'.repeat(32),
    },
    store,
    request as typeof fetch,
  );
}

function oauthRequests() {
  return vi.fn(async (input: string | URL | Request) => {
    const url = String(input);
    if (url.includes('/login/oauth/access_token')) {
      return Response.json({ access_token: 'temporary-token' });
    }
    if (url === 'https://api.github.com/user') {
      return Response.json({ id: 42, login: 'maintainer' });
    }
    throw new Error(`unexpected request: ${url}`);
  });
}

function signedPayload(value: string): Record<string, unknown> {
  const [payload] = value.split('.');
  if (!payload) throw new Error('state payload is missing');
  return JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as Record<string, unknown>;
}

function signedSession(payload: Record<string, unknown>, secret: string): string {
  const encoded = Buffer.from(JSON.stringify(payload)).toString('base64url');
  const signature = createHmac('sha256', secret).update(encoded).digest('base64url');
  return `${encoded}.${signature}`;
}
