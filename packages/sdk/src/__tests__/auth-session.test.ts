import { describe, expect, it } from 'vitest';
import { SignJWT } from 'jose';
import { SESSION_ISSUER, sessionSecret, verifySessionJwt } from '../auth/session.js';

const ADDRESS = '11111111111111111111111111111111';
const secret = new TextEncoder().encode('a'.repeat(32));

async function sign(
  claims: { issuer?: string; subject?: string; expiration?: number | string | null } = {},
  signWith: Uint8Array = secret,
): Promise<string> {
  let builder = new SignJWT({})
    .setProtectedHeader({ alg: 'HS256' })
    .setIssuer(claims.issuer ?? SESSION_ISSUER);
  if (claims.subject !== undefined) builder = builder.setSubject(claims.subject);
  if (claims.expiration !== null) builder = builder.setExpirationTime(claims.expiration ?? '5m');
  return builder.sign(signWith);
}

describe('sessionSecret', () => {
  it('encodes a provided secret to its UTF-8 bytes', () => {
    expect(sessionSecret('hunter2')).toEqual(new TextEncoder().encode('hunter2'));
  });

  it('throws when the secret is missing or empty', () => {
    expect(() => sessionSecret('')).toThrow('SESSION_SECRET is required');
  });
});

describe('verifySessionJwt', () => {
  it('returns the address and expiry for a valid session token', async () => {
    const payload = await verifySessionJwt(await sign({ subject: ADDRESS }), secret);
    expect(payload?.address).toBe(ADDRESS);
    expect(typeof payload?.expiresAt).toBe('number');
  });

  it('fails closed to null when the signature does not match the secret', async () => {
    const token = await sign({ subject: ADDRESS }, new TextEncoder().encode('b'.repeat(32)));
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('fails closed to null for the wrong issuer', async () => {
    const token = await sign({ issuer: 'covenant.evil', subject: ADDRESS });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('fails closed to null for an expired token', async () => {
    const token = await sign({ subject: ADDRESS, expiration: Math.floor(Date.now() / 1000) - 60 });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });

  it('rejects a subject that is not a Solana address', async () => {
    expect(await verifySessionJwt(await sign({ subject: 'not-an-address' }), secret)).toBeNull();
  });

  it('rejects a token with no subject claim', async () => {
    expect(await verifySessionJwt(await sign({}), secret)).toBeNull();
  });

  it('rejects a token with no numeric expiry claim', async () => {
    const token = await sign({ subject: ADDRESS, expiration: null });
    expect(await verifySessionJwt(token, secret)).toBeNull();
  });
});
