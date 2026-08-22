import { describe, expect, it } from 'vitest';
import {
  buildWalletProofMessage,
  consumeWalletProofSession,
  expireWalletProofSession,
  issueWalletProofSession,
  revokeWalletProofSession,
  verifyWalletProofSession,
  type WalletProofSession,
} from './wallet-proof.js';
import { DomainRuleError } from './state-machine.js';

const T0 = '2026-08-22T10:00:00.000Z';

function issued(): WalletProofSession {
  return issueWalletProofSession({
    id: 'wallet-session-1',
    githubUserId: 'github:42',
    domain: 'mizuki.example',
    uri: 'https://mizuki.example/contributors/wallet',
    nonce: 'abcdefghijklmnop',
    at: T0,
  });
}

describe('wallet proof sessions', () => {
  it('issues a domain-bound session with a 15-minute default lifetime', () => {
    expect(issued()).toMatchObject({
      state: 'issued',
      domain: 'mizuki.example',
      issuedAt: T0,
      expiresAt: '2026-08-22T10:15:00.000Z',
      revision: 0,
    });
  });

  it('builds a deterministic nonce-bound sign-in message', () => {
    const message = buildWalletProofMessage(issued(), 'wallet-1');
    expect(message).toContain('mizuki.example wants you to sign in');
    expect(message).toContain('Nonce: abcdefghijklmnop');
    expect(message).toContain('Request ID: wallet-session-1');
    expect(message).toContain('GitHub User ID: github:42');
  });

  it('records an externally verified signature without retaining the raw signature', () => {
    const session = issued();
    const verified = verifyWalletProofSession(session, {
      walletAddress: 'wallet-1',
      signedMessage: buildWalletProofMessage(session, 'wallet-1'),
      signature: 'signature-1',
      signatureVerified: true,
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 0,
    });
    expect(verified).toMatchObject({
      state: 'verified',
      walletAddress: 'wallet-1',
      verifiedAt: '2026-08-22T10:05:00.000Z',
      revision: 1,
    });
    expect(verified.proofFingerprint).toMatch(/^[a-f0-9]{64}$/);
    expect(verified).not.toHaveProperty('signature');

    const consumed = consumeWalletProofSession(verified, {
      at: '2026-08-22T10:06:00.000Z',
      expectedRevision: 1,
    });
    expect(consumed).toMatchObject({
      state: 'consumed',
      consumedAt: '2026-08-22T10:06:00.000Z',
      revision: 2,
    });
  });

  it('rejects failed verification and message substitution', () => {
    const session = issued();
    expect(() =>
      verifyWalletProofSession(session, {
        walletAddress: 'wallet-1',
        signedMessage: buildWalletProofMessage(session, 'wallet-1'),
        signature: 'signature-1',
        signatureVerified: false,
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 0,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_WALLET_SIGNATURE',
      }),
    );
    expect(() =>
      verifyWalletProofSession(session, {
        walletAddress: 'wallet-2',
        signedMessage: buildWalletProofMessage(session, 'wallet-1'),
        signature: 'signature-1',
        signatureVerified: true,
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 0,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'MESSAGE_MISMATCH',
      }),
    );
  });

  it('expires at the exact deadline and cannot be consumed afterward', () => {
    const session = issued();
    expect(() =>
      verifyWalletProofSession(session, {
        walletAddress: 'wallet-1',
        signedMessage: buildWalletProofMessage(session, 'wallet-1'),
        signature: 'signature-1',
        signatureVerified: true,
        at: '2026-08-22T10:15:00.000Z',
        expectedRevision: 0,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'SESSION_EXPIRED',
      }),
    );
    const expired = expireWalletProofSession(session, {
      at: '2026-08-22T10:15:00.000Z',
      expectedRevision: 0,
    });
    expect(expired.state).toBe('expired');
    expect(() =>
      consumeWalletProofSession(expired, {
        at: '2026-08-22T10:16:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrow();
  });

  it('supports revocation before consumption', () => {
    const revoked = revokeWalletProofSession(issued(), {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    expect(revoked).toMatchObject({ state: 'revoked', revision: 1 });
  });

  it('does not allow session events to move backward in time', () => {
    const session = issued();
    const verified = verifyWalletProofSession(session, {
      walletAddress: 'wallet-1',
      signedMessage: buildWalletProofMessage(session, 'wallet-1'),
      signature: 'signature-1',
      signatureVerified: true,
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 0,
    });
    expect(() =>
      consumeWalletProofSession(verified, {
        at: '2026-08-22T10:04:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'TIME_MOVED_BACKWARD',
      }),
    );
  });

  it('rejects weak nonces, insecure remote URIs, and mismatched domains', () => {
    expect(() =>
      issueWalletProofSession({
        id: '1',
        githubUserId: '42',
        domain: 'mizuki.example',
        uri: 'https://mizuki.example',
        nonce: 'short',
        at: T0,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_NONCE' }));
    expect(() =>
      issueWalletProofSession({
        id: '1',
        githubUserId: '42',
        domain: 'mizuki.example',
        uri: 'http://mizuki.example',
        nonce: 'abcdefghijklmnop',
        at: T0,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_URI' }));
    expect(() =>
      issueWalletProofSession({
        id: '1',
        githubUserId: '42',
        domain: 'mizuki.example',
        uri: 'https://other.example',
        nonce: 'abcdefghijklmnop',
        at: T0,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'DOMAIN_MISMATCH' }));
  });
});
