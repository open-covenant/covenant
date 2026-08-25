import { generateKeyPairSync, sign } from 'node:crypto';
import { getBase58Decoder } from '@solana/kit';
import { describe, expect, it, vi } from 'vitest';
import { ContributorAuth, GithubOAuthCallbackError } from './auth.js';
import { MemoryStore } from './store.js';

describe('ContributorAuth', () => {
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
