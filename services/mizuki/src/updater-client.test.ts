import { describe, expect, it, vi } from 'vitest';
import { UpdaterStatusClient } from './updater-client.js';

const RESPONSE = {
  upgrade: {
    id: '11111111-1111-4111-8111-111111111111',
    proposalId: 'proposal-1',
    sourceHandoffSha256: '9'.repeat(64),
    manifestSha256: 'a'.repeat(64),
    artifactSha256: 'b'.repeat(64),
    repository: {
      owner: 'mizuki-labs',
      name: 'mizuki',
      baseBranch: 'main',
      headBranch: 'mizuki/proposal-1',
    },
    candidateSha: 'c'.repeat(40),
    attestations: {
      proposal: { keyId: 'proposal-key', sha256: 'a'.repeat(64) },
      benchmark: {
        receiptId: 'benchmark-1',
        keyId: 'benchmark-key',
        sha256: 'd'.repeat(64),
      },
      review: {
        receiptId: 'review-1',
        keyId: 'review-key',
        sha256: 'e'.repeat(64),
      },
    },
    state: 'proposal_verified',
    prNumber: null,
    prUrl: null,
    deploymentId: null,
    mergeSha: null,
    promotionOperationId: null,
    promotionHealthyAt: null,
    nextAttemptAt: null,
    lastError: null,
    createdAt: '2026-08-22T12:00:00.000Z',
    updatedAt: '2026-08-22T12:01:00.000Z',
  },
  auditHeadHash: 'f'.repeat(64),
};

describe('UpdaterStatusClient', () => {
  it('authenticates and parses evidence by proposal id', async () => {
    const request = vi.fn<typeof fetch>(async () => Response.json(RESPONSE));
    const client = new UpdaterStatusClient('https://updater.example/', 'secret', 1_000, request);

    await expect(client.getByProposalId('proposal-1')).resolves.toMatchObject({
      proposalId: 'proposal-1',
      sourceHandoffSha256: '9'.repeat(64),
      state: 'proposal_verified',
      auditHeadHash: 'f'.repeat(64),
    });
    expect(request).toHaveBeenCalledWith(
      'https://updater.example/v1/proposals/proposal-1',
      expect.objectContaining({
        headers: expect.objectContaining({ authorization: 'Bearer secret' }),
      }),
    );
  });

  it('treats an unknown proposal as not yet submitted', async () => {
    const request = vi.fn<typeof fetch>(async () => new Response(null, { status: 404 }));
    const client = new UpdaterStatusClient('https://updater.example', 'secret', 1_000, request);
    await expect(client.getByProposalId('proposal-1')).resolves.toBeUndefined();
  });

  it('fails closed on malformed updater evidence', async () => {
    const request = vi.fn<typeof fetch>(async () =>
      Response.json({ ...RESPONSE, auditHeadHash: 'not-a-hash' }),
    );
    const client = new UpdaterStatusClient('https://updater.example', 'secret', 1_000, request);
    await expect(client.getByProposalId('proposal-1')).rejects.toThrow();
  });

  it('authenticates and strictly validates live updater health', async () => {
    const request = vi.fn<typeof fetch>(async (input) =>
      String(input).endsWith('/health')
        ? Response.json({ status: 'ok', service: 'mizuki-updater' })
        : new Response(null, { status: 404 }),
    );
    const client = new UpdaterStatusClient('https://updater.example', 'secret', 1_000, request);
    await expect(client.readiness()).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://updater.example/health',
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
    expect(request).toHaveBeenCalledWith(
      'https://updater.example/v1/proposals/mizuki-readiness-probe',
      expect.objectContaining({
        headers: expect.objectContaining({ authorization: 'Bearer secret' }),
      }),
    );

    request.mockImplementationOnce(async () => Response.json({ status: 'ok', service: 'other' }));
    await expect(client.readiness()).rejects.toThrow();
  });

  it('fails closed when the updater omits the signed source handoff hash', async () => {
    const response = structuredClone(RESPONSE) as Record<string, unknown>;
    delete (response.upgrade as Record<string, unknown>).sourceHandoffSha256;
    const request = vi.fn<typeof fetch>(async () => Response.json(response));
    const client = new UpdaterStatusClient('https://updater.example', 'secret', 1_000, request);

    await expect(client.getByProposalId('proposal-1')).rejects.toThrow();
  });
});
