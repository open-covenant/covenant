import { createHash } from 'node:crypto';
import {
  DomainRuleError,
  addHours,
  assertExpectedRevision,
  assertNonEmpty,
  assertNotBefore,
  assertTransition,
  timestampMs,
  type TransitionTable,
} from './state-machine.js';

export const WALLET_PROOF_TTL_MINUTES = 15;

export type WalletProofState = 'issued' | 'verified' | 'consumed' | 'expired' | 'revoked';

export type WalletProofSession = {
  id: string;
  githubUserId: string;
  domain: string;
  uri: string;
  nonce: string;
  state: WalletProofState;
  issuedAt: string;
  expiresAt: string;
  updatedAt: string;
  walletAddress?: string;
  proofFingerprint?: string;
  verifiedAt?: string;
  consumedAt?: string;
  revokedAt?: string;
  revision: number;
};

const transitions: TransitionTable<WalletProofState> = {
  issued: ['verified', 'expired', 'revoked'],
  verified: ['consumed', 'expired', 'revoked'],
  consumed: [],
  expired: [],
  revoked: [],
};

export function issueWalletProofSession(input: {
  id: string;
  githubUserId: string;
  domain: string;
  uri: string;
  nonce: string;
  at: string;
  ttlMinutes?: number;
}): WalletProofSession {
  const domain = normalizeDomain(input.domain);
  const uri = validateUri(input.uri, domain);
  const nonce = assertNonEmpty(input.nonce, 'nonce');
  if (nonce.length < 16 || nonce.length > 128 || !/^[A-Za-z0-9_-]+$/.test(nonce)) {
    throw new DomainRuleError('INVALID_NONCE', 'Nonce must be 16-128 URL-safe characters');
  }
  const ttlMinutes = input.ttlMinutes ?? WALLET_PROOF_TTL_MINUTES;
  if (!Number.isInteger(ttlMinutes) || ttlMinutes < 1 || ttlMinutes > 30) {
    throw new DomainRuleError('INVALID_SESSION_TTL', 'Wallet proof TTL must be 1-30 minutes');
  }
  const issuedAt = new Date(timestampMs(input.at, 'issued at')).toISOString();
  const expiresAt = addHours(issuedAt, ttlMinutes / 60);

  return {
    id: assertNonEmpty(input.id, 'session id'),
    githubUserId: assertNonEmpty(input.githubUserId, 'GitHub user id'),
    domain,
    uri,
    nonce,
    state: 'issued',
    issuedAt,
    expiresAt,
    updatedAt: issuedAt,
    revision: 0,
  };
}

export function buildWalletProofMessage(
  session: WalletProofSession,
  walletAddressValue: string,
): string {
  const walletAddress = assertNonEmpty(walletAddressValue, 'wallet address');
  return [
    `${session.domain} wants you to sign in with your Solana account:`,
    walletAddress,
    '',
    'Authorize this wallet for Mizuki contributor payments.',
    '',
    `URI: ${session.uri}`,
    'Version: 1',
    'Chain ID: solana:mainnet',
    `Nonce: ${session.nonce}`,
    `Issued At: ${session.issuedAt}`,
    `Expiration Time: ${session.expiresAt}`,
    `Request ID: ${session.id}`,
    `GitHub User ID: ${session.githubUserId}`,
  ].join('\n');
}

export function verifyWalletProofSession(
  session: WalletProofSession,
  input: {
    walletAddress: string;
    signedMessage: string;
    signature: string;
    signatureVerified: boolean;
    at: string;
    expectedRevision: number;
  },
): WalletProofSession {
  assertExpectedRevision(session.revision, input.expectedRevision);
  assertNotBefore(input.at, session.updatedAt, 'verification time');
  assertTransition(transitions, session.state, 'verified', 'Wallet proof session');
  assertSessionActive(session, input.at);
  if (!input.signatureVerified) {
    throw new DomainRuleError('INVALID_WALLET_SIGNATURE', 'Wallet signature was not verified');
  }

  const walletAddress = assertNonEmpty(input.walletAddress, 'wallet address');
  const expectedMessage = buildWalletProofMessage(session, walletAddress);
  if (input.signedMessage !== expectedMessage) {
    throw new DomainRuleError('MESSAGE_MISMATCH', 'Signed message does not match the session');
  }
  const signature = assertNonEmpty(input.signature, 'signature');
  const verifiedAt = new Date(timestampMs(input.at)).toISOString();

  return {
    ...session,
    state: 'verified',
    walletAddress,
    proofFingerprint: createHash('sha256')
      .update(`${expectedMessage}\u0000${signature}`)
      .digest('hex'),
    verifiedAt,
    updatedAt: verifiedAt,
    revision: session.revision + 1,
  };
}

export function consumeWalletProofSession(
  session: WalletProofSession,
  input: { at: string; expectedRevision: number },
): WalletProofSession {
  assertExpectedRevision(session.revision, input.expectedRevision);
  assertNotBefore(input.at, session.updatedAt, 'consumption time');
  assertTransition(transitions, session.state, 'consumed', 'Wallet proof session');
  assertSessionActive(session, input.at);
  const consumedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...session,
    state: 'consumed',
    consumedAt,
    updatedAt: consumedAt,
    revision: session.revision + 1,
  };
}

export function expireWalletProofSession(
  session: WalletProofSession,
  input: { at: string; expectedRevision: number },
): WalletProofSession {
  assertExpectedRevision(session.revision, input.expectedRevision);
  assertNotBefore(input.at, session.updatedAt, 'expiry time');
  assertTransition(transitions, session.state, 'expired', 'Wallet proof session');
  if (timestampMs(input.at) < timestampMs(session.expiresAt)) {
    throw new DomainRuleError('SESSION_STILL_ACTIVE', 'Wallet proof session has not expired');
  }
  return {
    ...session,
    state: 'expired',
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: session.revision + 1,
  };
}

export function revokeWalletProofSession(
  session: WalletProofSession,
  input: { at: string; expectedRevision: number },
): WalletProofSession {
  assertExpectedRevision(session.revision, input.expectedRevision);
  assertNotBefore(input.at, session.updatedAt, 'revocation time');
  assertTransition(transitions, session.state, 'revoked', 'Wallet proof session');
  const revokedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...session,
    state: 'revoked',
    revokedAt,
    updatedAt: revokedAt,
    revision: session.revision + 1,
  };
}

function assertSessionActive(session: WalletProofSession, at: string): void {
  if (timestampMs(at) >= timestampMs(session.expiresAt)) {
    throw new DomainRuleError('SESSION_EXPIRED', 'Wallet proof session has expired');
  }
}

function normalizeDomain(value: string): string {
  const domain = assertNonEmpty(value, 'domain').toLowerCase();
  if (domain.includes('://') || !/^[a-z0-9.-]+(?::[0-9]+)?$/.test(domain)) {
    throw new DomainRuleError('INVALID_DOMAIN', 'Domain must be a host without a scheme or path');
  }
  return domain;
}

function validateUri(value: string, domain: string): string {
  let uri: URL;
  try {
    uri = new URL(value);
  } catch {
    throw new DomainRuleError('INVALID_URI', 'URI must be a valid URL');
  }
  const localhost = uri.hostname === 'localhost' || uri.hostname === '127.0.0.1';
  if ((!localhost && uri.protocol !== 'https:') || (localhost && uri.protocol !== 'http:')) {
    throw new DomainRuleError('INVALID_URI', 'URI must use HTTPS, except on localhost');
  }
  if (uri.host.toLowerCase() !== domain) {
    throw new DomainRuleError('DOMAIN_MISMATCH', 'URI host must match the proof domain');
  }
  return uri.toString();
}
