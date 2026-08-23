import { afterEach, describe, expect, it, vi } from 'vitest';
import { sha256Hex, UpdaterError } from './domain.js';
import { proposalFixture } from './test-utils.js';
import { HttpArtifactVerifier, ProposalVerifier } from './verification.js';

const NOW = new Date('2026-08-22T12:00:00.000Z');
const imageCommit = 'a'.repeat(40);
const releaseUrl =
  `https://github.com/open-covenant/covenant/releases/download/mizuki-image-${imageCommit}` +
  '/manifest.oci.json';
const releaseAssetOrigin = 'https://release-assets.githubusercontent.com';

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

  it('requires exactly the GitHub release and release asset origins', () => {
    expect(() => new HttpArtifactVerifier(new Set(['https://github.com']), 1_000, 1_024)).toThrow(
      'Artifact origins must be the GitHub release and release asset origins',
    );
    expect(
      () =>
        new HttpArtifactVerifier(
          new Set([
            'https://github.com',
            releaseAssetOrigin,
            'https://objects.githubusercontent.com',
          ]),
          1_000,
          1_024,
        ),
    ).toThrow('Artifact origins must be the GitHub release and release asset origins');
  });

  it('follows the exact release redirect and verifies size and SHA-256', async () => {
    const artifact = Buffer.from('artifact bytes');
    const fetch = releaseFetch(
      releaseAssetUrl(),
      new Response(artifact, {
        status: 200,
        headers: { 'content-length': String(artifact.length) },
      }),
    );
    vi.stubGlobal('fetch', fetch);
    const verifier = createArtifactVerifier();
    await expect(
      verifier.verify(releaseUrl, sha256Hex(artifact), artifact.length),
    ).resolves.toEqual({ sha256: sha256Hex(artifact), sizeBytes: artifact.length });
    expect(fetch).toHaveBeenCalledTimes(2);
    for (const [, init] of fetch.mock.calls) {
      expect(new Headers(init?.headers).has('authorization')).toBe(false);
      expect(init?.redirect).toBe('manual');
    }
  });

  it('rejects wrong repository, tag, asset, query, credentials, and direct CDN input', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    const invalid = [
      releaseUrl.replace('open-covenant/covenant', 'open-covenant/elsewhere'),
      releaseUrl.replace(`mizuki-image-${imageCommit}`, 'mizuki-image-v1'),
      releaseUrl.replace('manifest.oci.json', 'promotion-input.json'),
      `${releaseUrl}?download=1`,
      `${releaseUrl}#fragment`,
      releaseUrl.replace('https://', 'https://token@'),
      releaseAssetUrl(),
    ];

    for (const value of invalid) {
      await expect(createArtifactVerifier().verify(value, '0'.repeat(64), 1)).rejects.toMatchObject(
        { code: 'artifact_origin_not_allowed' },
      );
    }
    expect(fetch).not.toHaveBeenCalled();
  });

  it('rejects malicious redirects and a second hop', async () => {
    const invalid = [
      releaseAssetUrl().replace(releaseAssetOrigin, 'https://objects.githubusercontent.com'),
      releaseAssetUrl().replace('/1219904470/', '/212613049/'),
      releaseAssetUrl().replace(
        'attachment%3B+filename%3Dmanifest.oci.json',
        'attachment%3B+filename%3Dpromotion-input.json',
      ),
      releaseAssetUrl().replace('sp=r&', ''),
      `${releaseAssetUrl()}#fragment`,
      releaseAssetUrl().replace('https://', 'https://token@'),
    ];
    const fetch = vi.fn();
    for (const location of invalid) {
      fetch.mockResolvedValueOnce(new Response(null, { status: 302, headers: { location } }));
    }
    vi.stubGlobal('fetch', fetch);
    for (const _location of invalid) {
      await expect(
        createArtifactVerifier().verify(releaseUrl, '0'.repeat(64), 1),
      ).rejects.toMatchObject({ code: 'artifact_redirect_invalid' });
    }

    vi.stubGlobal(
      'fetch',
      releaseFetch(
        releaseAssetUrl(),
        new Response(null, { status: 302, headers: { location: releaseAssetUrl() } }),
      ),
    );
    await expect(
      createArtifactVerifier().verify(releaseUrl, '0'.repeat(64), 1),
    ).rejects.toMatchObject({ code: 'artifact_redirect_limit', retryable: false });
  });

  it('rejects incorrect size and incorrect hashes', async () => {
    const artifact = Buffer.from('artifact bytes');
    vi.stubGlobal(
      'fetch',
      releaseFetch(releaseAssetUrl(), new Response(artifact, { status: 200 })),
    );
    await expect(
      createArtifactVerifier().verify(releaseUrl, sha256Hex(artifact), artifact.length + 1),
    ).rejects.toMatchObject({ code: 'artifact_size_mismatch' });

    vi.stubGlobal(
      'fetch',
      releaseFetch(releaseAssetUrl(), new Response(artifact, { status: 200 })),
    );
    await expect(
      createArtifactVerifier().verify(releaseUrl, '0'.repeat(64), artifact.length),
    ).rejects.toMatchObject({ code: 'artifact_hash_mismatch' });
  });

  it('classifies initial and final artifact server failures as retryable', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('failed', { status: 503 })),
    );
    await expect(createArtifactVerifier().verify(releaseUrl, '0'.repeat(64), 1)).rejects.toEqual(
      expect.objectContaining<Partial<UpdaterError>>({ retryable: true }),
    );

    vi.stubGlobal(
      'fetch',
      releaseFetch(releaseAssetUrl(), new Response('failed', { status: 503 })),
    );
    await expect(createArtifactVerifier().verify(releaseUrl, '0'.repeat(64), 1)).rejects.toEqual(
      expect.objectContaining<Partial<UpdaterError>>({ retryable: true }),
    );
  });
});

function createArtifactVerifier(): HttpArtifactVerifier {
  return new HttpArtifactVerifier(
    new Set(['https://github.com', releaseAssetOrigin]),
    1_000,
    1_024,
  );
}

function releaseFetch(location: string, asset: Response) {
  return vi
    .fn<typeof fetch>()
    .mockResolvedValueOnce(new Response(null, { status: 302, headers: { location } }))
    .mockResolvedValueOnce(asset);
}

function releaseAssetUrl(): string {
  const query = new URLSearchParams({
    sp: 'r',
    sv: '2018-11-09',
    sr: 'b',
    spr: 'https',
    se: '2026-08-23T20:00:00Z',
    rscd: 'attachment; filename=manifest.oci.json',
    rsct: 'application/octet-stream',
    sig: 'signed-value',
    'response-content-disposition': 'attachment; filename=manifest.oci.json',
    'response-content-type': 'application/octet-stream',
  });
  return (
    `${releaseAssetOrigin}/github-production-release-asset/1219904470/` +
    `123e4567-e89b-12d3-a456-426614174000?${query}`
  );
}

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
