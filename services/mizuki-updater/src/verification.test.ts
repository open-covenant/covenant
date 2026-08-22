import { afterEach, describe, expect, it, vi } from 'vitest';
import { sha256Hex, UpdaterError } from './domain.js';
import { proposalFixture } from './test-utils.js';
import { HttpArtifactVerifier, ProposalVerifier } from './verification.js';

const NOW = new Date('2026-08-22T12:00:00.000Z');

describe('proposal verification', () => {
  it('accepts a signed manifest with linked benchmark and independent review receipts', () => {
    const fixture = proposalFixture(NOW);
    const verifier = createVerifier(fixture);
    expect(() => verifier.verify(fixture.proposal, NOW)).not.toThrow();
  });

  it('rejects content changed after signing', () => {
    const fixture = proposalFixture(NOW);
    const verifier = createVerifier(fixture);
    fixture.proposal.manifest.title = 'tampered';
    expect(() => verifier.verify(fixture.proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'manifest_hash_mismatch' }),
    );
  });

  it('rejects receipt hashes not bound to their content', () => {
    const fixture = proposalFixture(NOW);
    const manifest = structuredClone(fixture.proposal.manifest);
    manifest.benchmark.sha256 = 'b'.repeat(64);
    const proposal = fixture.signManifest(manifest);
    expect(() => createVerifier(fixture).verify(proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'benchmark_hash_mismatch' }),
    );
  });

  it('rejects review performed by the implementing route', () => {
    const fixture = proposalFixture(NOW);
    const manifest = structuredClone(fixture.proposal.manifest);
    manifest.review.receipt.reviewerRoute = manifest.review.receipt.implementerRoute;
    fixture.attestReview(manifest);
    const proposal = fixture.signManifest(manifest);
    expect(() => createVerifier(fixture).verify(proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'review_not_independent' }),
    );
  });

  it('rejects a forged independent-review attestation even when the proposal is valid', () => {
    const fixture = proposalFixture(NOW);
    const manifest = structuredClone(fixture.proposal.manifest);
    manifest.review.signature = `${manifest.review.signature.slice(0, -4)}AAAA`;
    const proposal = fixture.signManifest(manifest);
    expect(() => createVerifier(fixture).verify(proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'invalid_review_signature' }),
    );
  });

  it('rejects a benchmark below its declared improvement threshold', () => {
    const fixture = proposalFixture(NOW);
    const manifest = structuredClone(fixture.proposal.manifest);
    manifest.benchmark.receipt.candidate = 85;
    fixture.attestBenchmark(manifest);
    const proposal = fixture.signManifest(manifest);
    expect(() => createVerifier(fixture).verify(proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'benchmark_not_improved' }),
    );
  });

  it('rejects expired proposals and repositories outside the allowlist', () => {
    const fixture = proposalFixture(NOW);
    const verifier = new ProposalVerifier({
      trustedProposalKeys: { 'release-key-1': fixture.publicKey },
      trustedBenchmarkKeys: { 'benchmark-key-1': fixture.benchmarkPublicKey },
      trustedReviewKeys: { 'review-key-1': fixture.reviewPublicKey },
      allowedRepositories: new Set(['another/repository']),
      allowedBaseBranches: new Set(['main']),
      headBranchPrefix: 'mizuki/',
      mandatoryChecks: new Set(['test', 'security']),
      maxProposalAgeMs: 60_000,
    });
    expect(() => verifier.verify(fixture.proposal, NOW)).toThrowError(
      expect.objectContaining({ code: 'repository_not_allowed' }),
    );

    expect(() =>
      createVerifier(fixture).verify(
        fixture.proposal,
        new Date(NOW.getTime() + 8 * 24 * 60 * 60_000),
      ),
    ).toThrowError(expect.objectContaining({ code: 'receipt_expired' }));
  });

  it('enforces operator-controlled branch and check policy', () => {
    const fixture = proposalFixture(NOW);
    const missingCheck = structuredClone(fixture.proposal.manifest);
    missingCheck.requiredChecks = ['test'];
    expect(() =>
      createVerifier(fixture).verify(fixture.signManifest(missingCheck), NOW),
    ).toThrowError(expect.objectContaining({ code: 'mandatory_check_missing' }));

    const wrongBase = structuredClone(fixture.proposal.manifest);
    wrongBase.repository.baseBranch = 'develop';
    expect(() => createVerifier(fixture).verify(fixture.signManifest(wrongBase), NOW)).toThrowError(
      expect.objectContaining({ code: 'base_branch_not_allowed' }),
    );
  });

  it('rejects trust stores that reuse one attestation key for multiple roles', () => {
    const fixture = proposalFixture(NOW);
    expect(
      () =>
        new ProposalVerifier({
          trustedProposalKeys: { 'release-key-1': fixture.publicKey },
          trustedBenchmarkKeys: { 'benchmark-key-1': fixture.benchmarkPublicKey },
          trustedReviewKeys: { 'review-key-1': fixture.publicKey },
          allowedRepositories: new Set(['mizuki-labs/mizuki']),
          allowedBaseBranches: new Set(['main']),
          headBranchPrefix: 'mizuki/',
          mandatoryChecks: new Set(['test', 'security']),
          maxProposalAgeMs: 7 * 24 * 60 * 60_000,
        }),
    ).toThrow('must not share keys');
  });
});

describe('artifact verification', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('streams an allowed artifact and verifies exact size and SHA-256', async () => {
    const artifact = Buffer.from('artifact bytes');
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(artifact, {
            status: 200,
            headers: { 'content-length': String(artifact.length) },
          }),
      ),
    );
    const verifier = new HttpArtifactVerifier(
      new Set(['https://artifacts.example.test']),
      1_000,
      1_024,
    );
    await expect(
      verifier.verify(
        'https://artifacts.example.test/candidate.tar',
        sha256Hex(artifact),
        artifact.length,
      ),
    ).resolves.toEqual({ sha256: sha256Hex(artifact), sizeBytes: artifact.length });
  });

  it('rejects disallowed origins, incorrect size, and incorrect hashes', async () => {
    const artifact = Buffer.from('artifact bytes');
    const verifier = new HttpArtifactVerifier(
      new Set(['https://artifacts.example.test']),
      1_000,
      1_024,
    );
    await expect(
      verifier.verify('https://elsewhere.test/a', sha256Hex(artifact), artifact.length),
    ).rejects.toMatchObject({ code: 'artifact_origin_not_allowed' });

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(artifact, { status: 200 })),
    );
    await expect(
      verifier.verify('https://artifacts.example.test/a', sha256Hex(artifact), artifact.length + 1),
    ).rejects.toMatchObject({ code: 'artifact_size_mismatch' });
    await expect(
      verifier.verify('https://artifacts.example.test/a', '0'.repeat(64), artifact.length),
    ).rejects.toMatchObject({ code: 'artifact_hash_mismatch' });
  });

  it('classifies artifact server failures as retryable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('failed', { status: 503 })),
    );
    const verifier = new HttpArtifactVerifier(
      new Set(['https://artifacts.example.test']),
      1_000,
      1_024,
    );
    await expect(
      verifier.verify('https://artifacts.example.test/a', '0'.repeat(64), 1),
    ).rejects.toEqual(expect.objectContaining<Partial<UpdaterError>>({ retryable: true }));
  });
});

function createVerifier(fixture: ReturnType<typeof proposalFixture>): ProposalVerifier {
  return new ProposalVerifier({
    trustedProposalKeys: { 'release-key-1': fixture.publicKey },
    trustedBenchmarkKeys: { 'benchmark-key-1': fixture.benchmarkPublicKey },
    trustedReviewKeys: { 'review-key-1': fixture.reviewPublicKey },
    allowedRepositories: new Set(['mizuki-labs/mizuki']),
    allowedBaseBranches: new Set(['main']),
    headBranchPrefix: 'mizuki/',
    mandatoryChecks: new Set(['test', 'security']),
    maxProposalAgeMs: 7 * 24 * 60 * 60_000,
  });
}
