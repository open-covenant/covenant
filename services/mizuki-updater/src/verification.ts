import { createHash, createPublicKey, verify } from 'node:crypto';
import type { KeyObject } from 'node:crypto';
import {
  hashObject,
  proposalSigningPayload,
  receiptSigningPayload,
  type SignedProposal,
  UpdaterError,
} from './domain.js';

export interface ProposalVerifierConfig {
  trustedProposalKeys: Record<string, string>;
  trustedBenchmarkKeys: Record<string, string>;
  trustedReviewKeys: Record<string, string>;
  allowedRepositories: Set<string>;
  allowedBaseBranches: Set<string>;
  headBranchPrefix: string;
  mandatoryChecks: Set<string>;
  maxProposalAgeMs: number;
}

export class ProposalVerifier {
  private readonly proposalKeys: Map<string, KeyObject>;
  private readonly benchmarkKeys: Map<string, KeyObject>;
  private readonly reviewKeys: Map<string, KeyObject>;

  constructor(private readonly config: ProposalVerifierConfig) {
    this.proposalKeys = parseKeys(config.trustedProposalKeys, 'proposal');
    this.benchmarkKeys = parseKeys(config.trustedBenchmarkKeys, 'benchmark');
    this.reviewKeys = parseKeys(config.trustedReviewKeys, 'review');
    assertSeparateKeySets(this.proposalKeys, this.benchmarkKeys, this.reviewKeys);
  }

  verify(proposal: SignedProposal, now = new Date()): void {
    const key = this.proposalKeys.get(proposal.keyId);
    if (!key) throw new UpdaterError('untrusted_key', 'Proposal key is not trusted', 403);

    const manifestHash = hashObject(proposal.manifest);
    if (manifestHash !== proposal.manifestSha256) {
      throw new UpdaterError('manifest_hash_mismatch', 'Manifest hash does not match its content');
    }

    const signature = Buffer.from(proposal.signature, 'base64');
    if (
      signature.length !== 64 ||
      !verify(null, proposalSigningPayload(proposal.keyId, manifestHash), key, signature)
    ) {
      throw new UpdaterError('invalid_signature', 'Proposal signature is invalid', 403);
    }

    const manifest = proposal.manifest;
    const repository = `${manifest.repository.owner}/${manifest.repository.name}`.toLowerCase();
    if (!this.config.allowedRepositories.has(repository)) {
      throw new UpdaterError(
        'repository_not_allowed',
        'Repository is not approved for upgrades',
        403,
      );
    }
    if (!this.config.allowedBaseBranches.has(manifest.repository.baseBranch)) {
      throw new UpdaterError('base_branch_not_allowed', 'Base branch is not approved', 403);
    }
    if (!manifest.repository.headBranch.startsWith(this.config.headBranchPrefix)) {
      throw new UpdaterError(
        'head_branch_not_allowed',
        'Candidate branch prefix is not approved',
        403,
      );
    }
    for (const check of this.config.mandatoryChecks) {
      if (!manifest.requiredChecks.includes(check)) {
        throw new UpdaterError(
          'mandatory_check_missing',
          `Manifest omits mandatory check: ${check}`,
        );
      }
    }

    this.verifyAge('proposal', manifest.issuedAt, now);
    this.verifyAge('benchmark receipt', manifest.benchmark.receipt.completedAt, now);
    this.verifyAge('review receipt', manifest.review.receipt.completedAt, now);
    const issuedAt = new Date(manifest.issuedAt).getTime();
    const benchmarkAt = new Date(manifest.benchmark.receipt.completedAt).getTime();
    const reviewAt = new Date(manifest.review.receipt.completedAt).getTime();
    if (benchmarkAt > reviewAt || benchmarkAt > issuedAt || reviewAt > issuedAt) {
      throw new UpdaterError(
        'invalid_receipt_chronology',
        'Benchmark must predate review, and both receipts must predate the proposal',
      );
    }

    if (hashObject(manifest.benchmark.receipt) !== manifest.benchmark.sha256) {
      throw new UpdaterError(
        'benchmark_hash_mismatch',
        'Benchmark receipt hash does not match its content',
      );
    }
    if (hashObject(manifest.review.receipt) !== manifest.review.sha256) {
      throw new UpdaterError(
        'review_hash_mismatch',
        'Review receipt hash does not match its content',
      );
    }
    this.verifyReceipt(
      'benchmark',
      manifest.benchmark.keyId,
      manifest.benchmark.sha256,
      manifest.benchmark.signature,
      this.benchmarkKeys,
    );
    this.verifyReceipt(
      'review',
      manifest.review.keyId,
      manifest.review.sha256,
      manifest.review.signature,
      this.reviewKeys,
    );

    const proposalFingerprint = keyFingerprint(key);
    if (
      proposalFingerprint === keyFingerprint(this.reviewKeys.get(manifest.review.keyId)!) ||
      proposalFingerprint === keyFingerprint(this.benchmarkKeys.get(manifest.benchmark.keyId)!) ||
      keyFingerprint(this.reviewKeys.get(manifest.review.keyId)!) ===
        keyFingerprint(this.benchmarkKeys.get(manifest.benchmark.keyId)!)
    ) {
      throw new UpdaterError(
        'attestation_keys_not_independent',
        'Proposal, benchmark, and review attestations must use separate keys',
      );
    }

    const benchmark = manifest.benchmark.receipt;
    const review = manifest.review.receipt;
    for (const receipt of [benchmark, review]) {
      if (receipt.candidateSha !== manifest.candidateSha) {
        throw new UpdaterError(
          'receipt_commit_mismatch',
          'Receipt does not cover the candidate commit',
        );
      }
      if (receipt.artifactSha256 !== manifest.artifact.sha256) {
        throw new UpdaterError('receipt_artifact_mismatch', 'Receipt does not cover the artifact');
      }
    }

    if (review.implementerRoute === review.reviewerRoute) {
      throw new UpdaterError(
        'review_not_independent',
        'Implementer and reviewer routes must differ',
      );
    }

    const improvement =
      benchmark.direction === 'increase'
        ? benchmark.candidate - benchmark.baseline
        : benchmark.baseline - benchmark.candidate;
    if (improvement < benchmark.minimumImprovement) {
      throw new UpdaterError('benchmark_not_improved', 'Candidate misses the benchmark threshold');
    }
  }

  private verifyAge(label: string, value: string, now: Date): void {
    const timestamp = new Date(value).getTime();
    const skewMs = timestamp - now.getTime();
    if (skewMs > 5 * 60_000) {
      throw new UpdaterError('receipt_from_future', `${label} timestamp is in the future`);
    }
    if (now.getTime() - timestamp > this.config.maxProposalAgeMs) {
      throw new UpdaterError('receipt_expired', `${label} is too old`);
    }
  }

  private verifyReceipt(
    kind: 'benchmark' | 'review',
    keyId: string,
    receiptHash: string,
    signatureValue: string,
    keys: Map<string, KeyObject>,
  ): void {
    const key = keys.get(keyId);
    if (!key) throw new UpdaterError(`untrusted_${kind}_key`, `${kind} key is not trusted`, 403);
    const signature = Buffer.from(signatureValue, 'base64');
    if (
      signature.length !== 64 ||
      !verify(null, receiptSigningPayload(kind, keyId, receiptHash), key, signature)
    ) {
      throw new UpdaterError(
        `invalid_${kind}_signature`,
        `${kind} receipt signature is invalid`,
        403,
      );
    }
  }
}

function parseKeys(values: Record<string, string>, purpose: string): Map<string, KeyObject> {
  const keys = new Map<string, KeyObject>();
  for (const [keyId, pem] of Object.entries(values)) {
    const key = createPublicKey(pem);
    if (key.asymmetricKeyType !== 'ed25519') {
      throw new Error(`Trusted ${purpose} key ${keyId} is not Ed25519`);
    }
    keys.set(keyId, key);
  }
  if (keys.size === 0) throw new Error(`At least one trusted ${purpose} key is required`);
  return keys;
}

function keyFingerprint(key: KeyObject): string {
  return createHash('sha256')
    .update(key.export({ type: 'spki', format: 'der' }))
    .digest('hex');
}

function assertSeparateKeySets(...sets: Array<Map<string, KeyObject>>): void {
  const fingerprints = new Set<string>();
  for (const keys of sets) {
    for (const key of keys.values()) {
      const fingerprint = keyFingerprint(key);
      if (fingerprints.has(fingerprint)) {
        throw new Error('Proposal, benchmark, and review trust stores must not share keys');
      }
      fingerprints.add(fingerprint);
    }
  }
}

export interface ArtifactVerification {
  sha256: string;
  sizeBytes: number;
}

export interface ArtifactVerifier {
  verify(url: string, expectedHash: string, expectedSize: number): Promise<ArtifactVerification>;
}

export class HttpArtifactVerifier implements ArtifactVerifier {
  constructor(
    private readonly allowedOrigins: Set<string>,
    private readonly timeoutMs: number,
    private readonly maxBytes: number,
  ) {
    if (allowedOrigins.size === 0) throw new Error('At least one artifact origin is required');
  }

  async verify(
    urlValue: string,
    expectedHash: string,
    expectedSize: number,
  ): Promise<ArtifactVerification> {
    const url = new URL(urlValue);
    if (url.protocol !== 'https:' || !this.allowedOrigins.has(url.origin)) {
      throw new UpdaterError('artifact_origin_not_allowed', 'Artifact origin is not approved', 403);
    }
    if (expectedSize > this.maxBytes) {
      throw new UpdaterError('artifact_too_large', 'Artifact exceeds the configured size limit');
    }

    let response: Response;
    try {
      response = await fetch(url, {
        headers: { accept: 'application/octet-stream' },
        redirect: 'manual',
        signal: AbortSignal.timeout(this.timeoutMs),
      });
    } catch (error) {
      throw new UpdaterError(
        'artifact_unavailable',
        error instanceof Error ? error.message : 'Artifact request failed',
        503,
        true,
      );
    }

    if (response.status >= 500) {
      throw new UpdaterError('artifact_unavailable', 'Artifact server failed', 503, true);
    }
    if (!response.ok || !response.body) {
      throw new UpdaterError(
        'artifact_unavailable',
        `Artifact request returned ${response.status}`,
      );
    }

    const length = response.headers.get('content-length');
    if (length !== null && Number(length) > this.maxBytes) {
      await response.body.cancel();
      throw new UpdaterError('artifact_too_large', 'Artifact exceeds the configured size limit');
    }

    const digest = createHash('sha256');
    const reader = response.body.getReader();
    let sizeBytes = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        sizeBytes += value.byteLength;
        if (sizeBytes > this.maxBytes || sizeBytes > expectedSize) {
          await reader.cancel();
          throw new UpdaterError('artifact_size_mismatch', 'Artifact size does not match manifest');
        }
        digest.update(value);
      }
    } catch (error) {
      if (error instanceof UpdaterError) throw error;
      throw new UpdaterError(
        'artifact_stream_failed',
        error instanceof Error ? error.message : 'Artifact stream failed',
        503,
        true,
      );
    }

    if (sizeBytes !== expectedSize) {
      throw new UpdaterError('artifact_size_mismatch', 'Artifact size does not match manifest');
    }
    const actualHash = digest.digest('hex');
    if (actualHash !== expectedHash) {
      throw new UpdaterError('artifact_hash_mismatch', 'Artifact hash does not match manifest');
    }
    return { sha256: actualHash, sizeBytes };
  }
}
