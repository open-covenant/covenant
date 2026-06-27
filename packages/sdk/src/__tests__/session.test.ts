import { describe, expect, it } from 'vitest';
import { SignJWT } from 'jose';
import { SESSION_ISSUER, sessionSecret, verifySessionJwt } from '../auth/session.js';

const secret = new TextEncoder().encode('unit-test-session-secret');
// System program id: 32-char base58, passes isSolanaAddress.
const VALID_ADDRESS = '11111111111111111111111111111111';

const sign = (payload: Record<string, unknown>, issuer = SESSION_ISSUER, signingSecret = secret) =>
  new SignJWT(payload).setProtectedHeader({ alg: 'HS256' }).setIssuer(issuer).sign(signingSecret);

const futureExp = () => Math.floor(Date.now() / 1000) + 3600;

describe('sessionSecret', () => {
  it('throws when the secret is missing or empty', () => {
    expect(() => sessionSecret(undefined)).toThrow('SESSION_SECRET is required');
    expect(() => sessionSecret('')).toThrow('SESSION_SECRET is required');
  });

  it('encodes a present secret to bytes', () => {
    expect(sessionSecret('hunter2')).toEqual(new TextEncoder().encode('hunter2'));
  });
});

describe('verifySessionJwt', () => {
  it('returns the address and expiry for a valid session token', async () => {
    const exp = futureExp();
    const token = await sign({ sub: VALID_ADDRESS, exp });
    expect(await verifySessionJwt(token, secret)).toEqual({ address: VALID_ADDRESS, expiresAt: exp });
  });

  it('returns null for a token signed by a different secret', async () => {
    const token = await sign({ sub: VALID_ADDRESS, exp: futureExp() }, SESSION_ISSUER, new TextEncoder().encode('a-different-secret'));
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('returns null for a token with the wrong issuer', async () => {
    const token = await sign({ sub: VALID_ADDRESS, exp: futureExp() }, 'evil.issuer');
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('returns null for a structurally garbage token', async () => {
    expect(await verifySessionJwt('not.a.jwt', secret)).toBeNull();
  });

  it('returns null when exp is absent', async () => {
    const token = await sign({ sub: VALID_ADDRESS });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('returns null when sub is not a Solana address', async () => {
    const token = await sign({ sub: 'not-a-solana-address', exp: futureExp() });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('returns null for an expired token', async () => {
    const token = await sign({ sub: VALID_ADDRESS, exp: Math.floor(Date.now() / 1000) - 10 });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });
});
