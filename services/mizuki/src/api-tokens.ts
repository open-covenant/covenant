import { createHash, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { API_TOKEN_SCOPES, type AccountApiToken, type ApiTokenScope } from './types.js';

export const API_TOKEN_VERSION_PREFIX = 'mzk_v1_';
export const API_TOKEN_MAX_ACTIVE = 20;
export const API_TOKEN_MAX_LIFETIME_DAYS = 365;

const LOOKUP_LENGTH = 12;
const tokenPattern = /^mzk_v1_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/;
const namePattern = /^[^\u0000-\u001f\u007f]{1,80}$/;

export type PublicApiToken = Omit<AccountApiToken, 'githubId' | 'tokenHash'> & {
  state: 'active' | 'expired' | 'revoked';
};

export type ApiTokenCredential = {
  token: string;
  record: AccountApiToken;
};

export function createApiToken(input: {
  githubId: string;
  name: string;
  scopes: readonly ApiTokenScope[];
  expiresAt: string;
  now?: Date;
}): ApiTokenCredential {
  const now = input.now ?? new Date();
  const name = input.name.trim();
  if (!namePattern.test(name)) {
    throw new ApiTokenInputError('Token name must be between 1 and 80 printable characters.');
  }
  const scopes = normalizedScopes(input.scopes);
  const expiresAt = validatedExpiration(input.expiresAt, now);
  const prefix = `${API_TOKEN_VERSION_PREFIX}${randomBytes(9).toString('base64url')}`;
  const token = `${prefix}_${randomBytes(32).toString('base64url')}`;
  return {
    token,
    record: {
      id: randomUUID(),
      githubId: input.githubId,
      name,
      prefix,
      tokenHash: tokenHash(token),
      scopes,
      expiresAt,
      createdAt: now.toISOString(),
    },
  };
}

export function apiTokenCandidate(
  value: string,
): { prefix: string; tokenHash: string } | undefined {
  if (!tokenPattern.test(value)) return undefined;
  return {
    prefix: value.slice(0, API_TOKEN_VERSION_PREFIX.length + LOOKUP_LENGTH),
    tokenHash: tokenHash(value),
  };
}

export function apiTokenHashMatches(stored: string, candidate: string): boolean {
  const expected = safeHashBytes(stored);
  const supplied = safeHashBytes(candidate);
  return (
    timingSafeEqual(expected, supplied) &&
    /^[a-f0-9]{64}$/.test(stored) &&
    /^[a-f0-9]{64}$/.test(candidate)
  );
}

export function publicApiToken(token: AccountApiToken, now = new Date()): PublicApiToken {
  const { githubId: _githubId, tokenHash: _tokenHash, ...metadata } = token;
  return { ...metadata, state: apiTokenState(token, now) };
}

export function apiTokenState(
  token: Pick<AccountApiToken, 'expiresAt' | 'revokedAt'>,
  now = new Date(),
): PublicApiToken['state'] {
  if (token.revokedAt) return 'revoked';
  return Date.parse(token.expiresAt) <= now.getTime() ? 'expired' : 'active';
}

export function normalizedScopes(values: readonly string[]): ApiTokenScope[] {
  const requested = new Set(values);
  if (requested.size !== values.length || requested.size === 0) {
    throw new ApiTokenInputError('Select at least one unique token scope.');
  }
  const scopes = API_TOKEN_SCOPES.filter((scope) => requested.delete(scope));
  if (requested.size > 0) throw new ApiTokenInputError('Token scope is not supported.');
  return scopes;
}

export class ApiTokenInputError extends Error {}

export class ApiTokenCapacityError extends Error {
  constructor() {
    super(`Each account can have up to ${API_TOKEN_MAX_ACTIVE} active API tokens.`);
  }
}

function validatedExpiration(value: string, now: Date): string {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/.test(value)) {
    throw new ApiTokenInputError('Token expiration must be an RFC 3339 UTC timestamp.');
  }
  const expires = new Date(value);
  const max = now.getTime() + API_TOKEN_MAX_LIFETIME_DAYS * 24 * 60 * 60_000;
  if (!Number.isFinite(expires.getTime()) || expires.getTime() <= now.getTime()) {
    throw new ApiTokenInputError('Token expiration must be in the future.');
  }
  if (expires.getTime() > max) {
    throw new ApiTokenInputError(
      `Token expiration cannot exceed ${API_TOKEN_MAX_LIFETIME_DAYS} days.`,
    );
  }
  return expires.toISOString();
}

function tokenHash(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}

function safeHashBytes(value: string): Buffer {
  return /^[a-f0-9]{64}$/.test(value) ? Buffer.from(value, 'hex') : Buffer.alloc(32);
}
