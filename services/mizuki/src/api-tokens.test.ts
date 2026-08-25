import { describe, expect, it } from 'vitest';
import {
  ApiTokenInputError,
  apiTokenCandidate,
  apiTokenHashMatches,
  createApiToken,
  normalizedScopes,
  publicApiToken,
} from './api-tokens.js';

const now = new Date('2026-08-25T10:00:00.000Z');

describe('account API tokens', () => {
  it('creates a high-entropy one-time value while retaining only prefix and hash', () => {
    const credential = createApiToken({
      githubId: '42',
      name: 'Release MCP',
      scopes: ['repositories:read', 'jobs:read'],
      expiresAt: '2026-11-23T10:00:00.000Z',
      now,
    });

    expect(credential.token).toMatch(/^mzk_v1_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/);
    expect(credential.record.prefix).toMatch(/^mzk_v1_[A-Za-z0-9_-]{12}$/);
    expect(credential.record.tokenHash).toMatch(/^[a-f0-9]{64}$/);
    expect(JSON.stringify(credential.record)).not.toContain(credential.token);
    expect(Buffer.from(credential.token.slice(-43), 'base64url')).toHaveLength(32);
    expect(apiTokenCandidate(credential.token)).toEqual({
      prefix: credential.record.prefix,
      tokenHash: credential.record.tokenHash,
    });
  });

  it('compares fixed-length hashes and rejects malformed token candidates', () => {
    const credential = createApiToken({
      githubId: '42',
      name: 'MCP',
      scopes: ['repositories:read'],
      expiresAt: '2026-09-24T10:00:00.000Z',
      now,
    });
    const candidate = apiTokenCandidate(credential.token)!;

    expect(apiTokenHashMatches(credential.record.tokenHash, candidate.tokenHash)).toBe(true);
    expect(apiTokenHashMatches(credential.record.tokenHash, 'f'.repeat(64))).toBe(false);
    expect(apiTokenHashMatches('malformed', candidate.tokenHash)).toBe(false);
    expect(apiTokenHashMatches('0'.repeat(64), 'malformed')).toBe(false);
    expect(apiTokenCandidate(`${credential.token}x`)).toBeUndefined();
    expect(apiTokenCandidate('mzk_v1_not-a-token')).toBeUndefined();
  });

  it('normalizes supported scopes and rejects duplicates, unknown values, and empty grants', () => {
    expect(normalizedScopes(['account:jobs:read', 'jobs:write', 'repositories:read'])).toEqual([
      'repositories:read',
      'jobs:write',
      'account:jobs:read',
    ]);
    expect(() => normalizedScopes([])).toThrow(ApiTokenInputError);
    expect(() => normalizedScopes(['jobs:read', 'jobs:read'])).toThrow(ApiTokenInputError);
    expect(() => normalizedScopes(['admin'])).toThrow(ApiTokenInputError);
  });

  it('bounds expiration and strips account and hash fields from public metadata', () => {
    expect(() =>
      createApiToken({
        githubId: '42',
        name: 'Ambiguous date',
        scopes: ['jobs:read'],
        expiresAt: '11/23/2026',
        now,
      }),
    ).toThrow('RFC 3339');
    expect(() =>
      createApiToken({
        githubId: '42',
        name: 'Expired',
        scopes: ['jobs:read'],
        expiresAt: now.toISOString(),
        now,
      }),
    ).toThrow('future');
    expect(() =>
      createApiToken({
        githubId: '42',
        name: 'Too long',
        scopes: ['jobs:read'],
        expiresAt: '2027-08-26T10:00:00.000Z',
        now,
      }),
    ).toThrow('365 days');

    const credential = createApiToken({
      githubId: '42',
      name: 'Metadata',
      scopes: ['jobs:read'],
      expiresAt: '2026-09-24T10:00:00.000Z',
      now,
    });
    const publicValue = publicApiToken(credential.record, now);
    expect(publicValue).toMatchObject({ name: 'Metadata', state: 'active' });
    expect(publicValue).not.toHaveProperty('githubId');
    expect(publicValue).not.toHaveProperty('tokenHash');
    expect(publicApiToken(credential.record, new Date(credential.record.expiresAt))).toMatchObject({
      state: 'expired',
    });
    credential.record.revokedAt = '2026-08-26T10:00:00.000Z';
    expect(publicApiToken(credential.record, now)).toMatchObject({ state: 'revoked' });
  });
});
