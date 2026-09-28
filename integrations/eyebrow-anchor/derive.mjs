// Derivation rules that turn an eyebrow lockfile into the three values the
// settlement program's `anchor_receipt_batch` takes: a 32-byte batch id, a
// 32-byte merkle root, and a receipt count.
//
// Everything here is SHA-256 over byte strings that the lockfile already
// contains. Nothing depends on this file: README.md states the same rules in
// prose, and a third party who has only the lockfile and a SHA-256
// implementation can reproduce every value below. That is the point. An
// anchor nobody else can recompute proves nothing.

import { createHash } from "node:crypto";

/** Domain separator for an artifact that carries no `contentHash`. */
export const ARTIFACT_DOMAIN = "eyebrow-artifact:v1\n";
/** Domain separator for the scan-level content hash. */
export const SCAN_DOMAIN = "eyebrow-scan-content:v1\n";
/** Domain separator for the batch id. */
export const BATCH_DOMAIN = "eyebrow-scan:v1:";

const sha256 = (...parts) => {
  const h = createHash("sha256");
  for (const part of parts) h.update(part);
  return h.digest();
};

const hex = (buf) => Buffer.from(buf).toString("hex");
const CONTENT_HASH_RE = /^sha256-([0-9a-f]{64})$/;

/**
 * The 32-byte digest that stands for one artifact.
 *
 * eyebrow hashes every file it can reach and folds them into the artifact's
 * `contentHash`. Remote MCP servers have no files to hash. eyebrow flags those
 * REMOTE-UNHASHABLE and pins the TLS SPKI instead, so they get a digest over
 * their identity and that pin. Both cases are 32 bytes, so the tree does not
 * care which kind a leaf is.
 */
export function artifactDigest(artifact) {
  if (typeof artifact?.id !== "string" || artifact.id.length === 0) {
    throw new Error("artifact has no id");
  }
  const declared = artifact.contentHash;
  if (typeof declared === "string" && declared.length > 0) {
    const match = CONTENT_HASH_RE.exec(declared);
    if (!match) {
      throw new Error(`artifact ${artifact.id}: unsupported contentHash "${declared}"`);
    }
    return Buffer.from(match[1], "hex");
  }
  const source = artifact.source ?? {};
  const preimage =
    ARTIFACT_DOMAIN +
    [
      artifact.id,
      artifact.tool ?? "",
      artifact.scope ?? "",
      artifact.type ?? "",
      artifact.name ?? "",
      source.kind ?? "",
      source.ref ?? "",
      source.certSpki ?? "",
    ].join("\n") +
    "\n";
  return sha256(Buffer.from(preimage, "utf8"));
}

/**
 * Artifacts in canonical order: ascending by the UTF-8 bytes of `id`.
 *
 * eyebrow already emits them sorted, but relying on that would make the anchor
 * depend on a scanner implementation detail rather than on the lockfile's
 * contents.
 */
export function canonicalLeaves(lockfile) {
  const artifacts = lockfile?.artifacts;
  if (!Array.isArray(artifacts)) throw new Error("lockfile has no artifacts array");
  if (artifacts.length === 0) throw new Error("lockfile has zero artifacts; nothing to anchor");

  const leaves = artifacts.map((artifact) => ({
    id: artifact.id,
    name: artifact.name ?? "",
    tool: artifact.tool ?? "",
    type: artifact.type ?? "",
    digest: artifactDigest(artifact),
    hashed: typeof artifact.contentHash === "string" && artifact.contentHash.length > 0,
  }));

  leaves.sort((a, b) => Buffer.compare(Buffer.from(a.id, "utf8"), Buffer.from(b.id, "utf8")));

  for (let i = 1; i < leaves.length; i += 1) {
    if (leaves[i].id === leaves[i - 1].id) {
      throw new Error(`lockfile has two artifacts with id ${leaves[i].id}`);
    }
  }
  return leaves;
}

/** Largest power of two strictly less than n, for n > 1. RFC 6962 section 2.1. */
function splitPoint(n) {
  return 1 << (31 - Math.clz32(n - 1));
}

/** RFC 6962 Merkle Tree Hash over a list of 32-byte entries. */
export function merkleRoot(digests) {
  if (digests.length === 0) return sha256(Buffer.alloc(0));
  if (digests.length === 1) return sha256(Buffer.from([0x00]), digests[0]);
  const k = splitPoint(digests.length);
  return sha256(Buffer.from([0x01]), merkleRoot(digests.slice(0, k)), merkleRoot(digests.slice(k)));
}

/**
 * RFC 6962 audit path for one leaf: the sibling hashes, bottom-up, that carry
 * that leaf to the root. This is what makes the anchor useful after the fact.
 * A reader can prove one skill or one MCP server was in the scan without
 * holding the other twenty-six.
 */
export function inclusionProof(digests, index) {
  if (index < 0 || index >= digests.length) throw new Error("leaf index out of range");
  if (digests.length === 1) return [];
  const k = splitPoint(digests.length);
  if (index < k) {
    return [...inclusionProof(digests.slice(0, k), index), merkleRoot(digests.slice(k))];
  }
  return [...inclusionProof(digests.slice(k), index - k), merkleRoot(digests.slice(0, k))];
}

/** Replay an audit path. Mirrors RFC 6962 section 2.1.1. */
export function verifyInclusion({ digest, index, leafCount, path, root }) {
  if (index < 0 || index >= leafCount) return false;
  let hash = sha256(Buffer.from([0x00]), digest);
  let fn = index;
  let sn = leafCount - 1;
  for (const sibling of path) {
    if (sn === 0) return false;
    if (fn % 2 === 1 || fn === sn) {
      hash = sha256(Buffer.from([0x01]), sibling, hash);
      while (fn % 2 === 0 && fn !== 0) {
        fn >>= 1;
        sn >>= 1;
      }
    } else {
      hash = sha256(Buffer.from([0x01]), hash, sibling);
    }
    fn >>= 1;
    sn >>= 1;
  }
  return sn === 0 && hash.equals(Buffer.from(root));
}

/**
 * The scan-level content hash.
 *
 * eyebrow 0.5.3 writes a `contentHash` per artifact and none for the lockfile
 * as a whole, so this defines one: a flat digest over the canonical artifact
 * list plus the two header fields that say which scanner ran and when. Two
 * scans of identical tooling taken at different times hash differently, which
 * is what lets a machine anchor its state again tomorrow.
 */
export function scanContentHash(lockfile, leaves) {
  let preimage =
    SCAN_DOMAIN +
    (lockfile.generator ?? "") +
    "\n" +
    (lockfile.generatedAt ?? "") +
    "\n" +
    String(leaves.length) +
    "\n";
  for (const leaf of leaves) preimage += `${leaf.id} ${hex(leaf.digest)}\n`;
  return sha256(Buffer.from(preimage, "utf8"));
}

/** batch_id = SHA-256("eyebrow-scan:v1:" + lowercase hex of the content hash). */
export function batchIdFor(contentHash) {
  return sha256(Buffer.from(BATCH_DOMAIN + hex(contentHash), "utf8"));
}

/** Everything the anchor and the verifier need, from a parsed lockfile. */
export function deriveFromLockfile(lockfile) {
  const leaves = canonicalLeaves(lockfile);
  const digests = leaves.map((leaf) => leaf.digest);
  const contentHash = scanContentHash(lockfile, leaves);
  const batchId = batchIdFor(contentHash);
  const root = merkleRoot(digests);
  if (leaves.length > 0xffffffff) throw new Error("more artifacts than a u32 receipt count holds");
  return {
    leaves,
    digests,
    contentHash,
    batchId,
    merkleRoot: root,
    receiptCount: leaves.length,
    generator: lockfile.generator ?? "",
    generatedAt: lockfile.generatedAt ?? "",
  };
}

export { hex, sha256 };
