// Pure, side-effect-free extraction of anchor-settlement.mjs's instruction
// construction. The PDA derivation needs @solana/web3.js and stays in the
// script; this module is node:crypto only, so it is unit-testable offline.
import { createHash } from "node:crypto";

// Anchor global discriminator for anchor_receipt_batch.
export const ANCHOR_DISCRIMINATOR = Buffer.from([90, 207, 91, 121, 57, 61, 176, 129]);

// batch_id = sha256("<sha>:<root>" utf8). The receipt-batch PDA is init-once
// on this value, so the canonical input string (commit-first order, ":"
// separator) is load-bearing: flipping it re-derives a free PDA and silently
// double-anchors a batch.
export function buildBatchId(sha, root) {
  return createHash("sha256").update(`${sha}:${root}`).digest();
}

// 4-byte little-endian u32 receipt count.
export function buildCountLe(count) {
  const buf = Buffer.alloc(4);
  buf.writeUInt32LE(count, 0);
  return buf;
}

export function assertRootLength(root) {
  if (Buffer.from(root, "hex").length !== 32) {
    throw new Error("root must be 32 bytes of hex");
  }
}

// anchor_receipt_batch instruction data:
// discriminator(8) || batch_id(32) || merkle_root(32) || count_le(4) = 76 bytes.
export function buildInstructionData(sha, root, count) {
  assertRootLength(root);
  const merkleRoot = Buffer.from(root, "hex");
  return Buffer.concat([ANCHOR_DISCRIMINATOR, buildBatchId(sha, root), merkleRoot, buildCountLe(count)]);
}
