import { generateKeyPairSync, sign } from 'node:crypto';
import {
  hashObject,
  proposalSigningPayload,
  receiptSigningPayload,
  sha256Hex,
  type SignedProposal,
  type UpgradeManifest,
} from './domain.js';

export function proposalFixture(now = new Date('2026-08-22T12:00:00.000Z')): {
  artifact: Buffer;
  publicKey: string;
  benchmarkPublicKey: string;
  reviewPublicKey: string;
  proposal: SignedProposal;
  signManifest(manifest: UpgradeManifest): SignedProposal;
  attestBenchmark(manifest: UpgradeManifest): void;
  attestReview(manifest: UpgradeManifest): void;
} {
  const artifact = Buffer.from('verified upgrade artifact');
  const artifactHash = sha256Hex(artifact);
  const candidateSha = 'a'.repeat(40);
  const benchmark = {
    version: 1 as const,
    receiptId: 'benchmark-1',
    candidateSha,
    artifactSha256: artifactHash,
    suite: 'maintenance-reliability',
    targetMetric: 'successful-jobs',
    direction: 'increase' as const,
    baseline: 80,
    candidate: 95,
    minimumImprovement: 10,
    protectedSuitePassed: true as const,
    completedAt: now.toISOString(),
  };
  const review = {
    version: 1 as const,
    receiptId: 'review-1',
    candidateSha,
    artifactSha256: artifactHash,
    implementerRoute: 'provider/implementer',
    reviewerRoute: 'provider/reviewer',
    verdict: 'approved' as const,
    blockingFindings: 0 as const,
    summary: 'Candidate satisfies the upgrade contract.',
    completedAt: now.toISOString(),
  };
  const manifest: UpgradeManifest = {
    version: 1,
    proposalId: 'upgrade-1',
    sourceHandoffSha256: 'f'.repeat(64),
    repository: {
      owner: 'mizuki-labs',
      name: 'mizuki',
      baseBranch: 'main',
      baseSha: 'b'.repeat(40),
      headBranch: 'mizuki/upgrade-1',
    },
    candidateSha,
    artifact: {
      url: 'https://artifacts.example.test/upgrade-1.tar.gz',
      sha256: artifactHash,
      sizeBytes: artifact.length,
    },
    title: 'improve maintenance reliability',
    body: 'Raises the protected maintenance benchmark.',
    requiredChecks: ['test', 'security'],
    benchmark: {
      receipt: benchmark,
      sha256: hashObject(benchmark),
      keyId: 'benchmark-key-1',
      signature: 'A'.repeat(86) + '==',
    },
    review: {
      receipt: review,
      sha256: hashObject(review),
      keyId: 'review-key-1',
      signature: 'A'.repeat(86) + '==',
    },
    issuedAt: now.toISOString(),
  };
  const pair = generateKeyPairSync('ed25519');
  const benchmarkPair = generateKeyPairSync('ed25519');
  const reviewPair = generateKeyPairSync('ed25519');
  const keyId = 'release-key-1';
  const publicKey = pair.publicKey.export({ type: 'spki', format: 'pem' }).toString();
  const benchmarkPublicKey = benchmarkPair.publicKey
    .export({ type: 'spki', format: 'pem' })
    .toString();
  const reviewPublicKey = reviewPair.publicKey.export({ type: 'spki', format: 'pem' }).toString();
  const attestBenchmark = (candidate: UpgradeManifest): void => {
    candidate.benchmark.sha256 = hashObject(candidate.benchmark.receipt);
    candidate.benchmark.signature = sign(
      null,
      receiptSigningPayload('benchmark', candidate.benchmark.keyId, candidate.benchmark.sha256),
      benchmarkPair.privateKey,
    ).toString('base64');
  };
  const attestReview = (candidate: UpgradeManifest): void => {
    candidate.review.sha256 = hashObject(candidate.review.receipt);
    candidate.review.signature = sign(
      null,
      receiptSigningPayload('review', candidate.review.keyId, candidate.review.sha256),
      reviewPair.privateKey,
    ).toString('base64');
  };
  attestBenchmark(manifest);
  attestReview(manifest);
  const signManifest = (candidate: UpgradeManifest): SignedProposal => {
    const manifestSha256 = hashObject(candidate);
    return {
      keyId,
      manifest: candidate,
      manifestSha256,
      signature: sign(
        null,
        proposalSigningPayload(keyId, manifestSha256),
        pair.privateKey,
      ).toString('base64'),
    };
  };
  return {
    artifact,
    publicKey,
    benchmarkPublicKey,
    reviewPublicKey,
    proposal: signManifest(manifest),
    signManifest,
    attestBenchmark,
    attestReview,
  };
}
