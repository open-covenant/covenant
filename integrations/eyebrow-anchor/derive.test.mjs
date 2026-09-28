#!/usr/bin/env node
// Tests for the derivation rules. No network, no keys.
//
// The merkle tests use the published RFC 6962 vectors rather than values this
// code produced, so a mistake here shows up as a mismatch against the standard
// instead of agreeing with itself.

import { createHash } from "node:crypto";
import {
  artifactDigest,
  batchIdFor,
  canonicalLeaves,
  deriveFromLockfile,
  hex,
  inclusionProof,
  merkleRoot,
  scanContentHash,
  verifyInclusion,
} from "./derive.mjs";

let failures = 0;
const check = (label, ok, detail) => {
  if (!ok) failures += 1;
  process.stdout.write(`${ok ? "ok  " : "FAIL"}  ${label}\n`);
  if (!ok && detail) process.stdout.write(`      ${detail}\n`);
};
const eq = (label, actual, expected) =>
  check(label, actual === expected, `got ${actual}, want ${expected}`);

// --------------------------------------------------------------- RFC 6962

const RFC6962_ENTRIES = [
  "",
  "00",
  "10",
  "2021",
  "3031",
  "40414243",
  "5051525354555657",
  "606162636465666768696a6b6c6d6e6f",
].map((entry) => Buffer.from(entry, "hex"));

const RFC6962_ROOTS = [
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
  "fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125",
  "aeb6bcfe274b70a14fb067a5e5578264db0fa9b51af5e0ba159158f329e06e77",
  "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
  "4e3bbb1f7b478dcfe71fb631631519a3bca12c9aefca1612bfce4c13a86264d4",
  "76e67dadbcdf1e10e1b74ddc608abd2f98dfb16fbce75277b5232a127f2087ef",
  "ddb89be403809e325750d3d263cd78929c2942b7942a34b77e122c9594a74c8c",
  "5dc9da79a70659a9ad559cb701ded9a2ab9d823aad2f4960cfe370eff4604328",
];

for (let n = 0; n <= 8; n += 1) {
  eq(`RFC 6962 root over ${n} entries`, hex(merkleRoot(RFC6962_ENTRIES.slice(0, n))), RFC6962_ROOTS[n]);
}

for (let n = 1; n <= 8; n += 1) {
  const entries = RFC6962_ENTRIES.slice(0, n);
  const root = merkleRoot(entries);
  let allOk = true;
  for (let i = 0; i < n; i += 1) {
    const path = inclusionProof(entries, i);
    if (!verifyInclusion({ digest: entries[i], index: i, leafCount: n, path, root })) allOk = false;
    // A proof must not verify against a different leaf index.
    for (let j = 0; j < n; j += 1) {
      if (j === i) continue;
      if (verifyInclusion({ digest: entries[i], index: j, leafCount: n, path, root })) allOk = false;
    }
  }
  check(`inclusion proofs round-trip and are index-bound for ${n} entries`, allOk);
}

// ------------------------------------------------------------- artifacts

const sha = (text) => createHash("sha256").update(text, "utf8").digest();

const artifactWithFiles = {
  id: "0f04932dbddbdd15",
  tool: "claude-code",
  scope: "global",
  type: "skill",
  name: "example-skill",
  source: { kind: "local", ref: "~/.claude/skills/example-skill" },
  files: [{ path: "SKILL.md", hash: "aa".repeat(32) }],
  contentHash: `sha256-${"11".repeat(32)}`,
};

const remoteArtifact = {
  id: "07bf01e342fa4980",
  tool: "codex",
  scope: "global",
  type: "mcp_server",
  name: "example-remote",
  source: { kind: "url", ref: "https://example.invalid/mcp", certSpki: "sha256/AAAA" },
};

eq("a declared contentHash becomes the leaf digest", hex(artifactDigest(artifactWithFiles)), "11".repeat(32));

eq(
  "an unhashable remote artifact hashes its identity and TLS pin",
  hex(artifactDigest(remoteArtifact)),
  hex(
    sha(
      "eyebrow-artifact:v1\n" +
        "07bf01e342fa4980\ncodex\nglobal\nmcp_server\nexample-remote\nurl\n" +
        "https://example.invalid/mcp\nsha256/AAAA\n",
    ),
  ),
);

check(
  "a malformed contentHash is rejected rather than silently reinterpreted",
  (() => {
    try {
      artifactDigest({ ...artifactWithFiles, contentHash: "blake3-abc" });
      return false;
    } catch {
      return true;
    }
  })(),
);

// -------------------------------------------------------------- lockfile

const lockfile = {
  version: 1,
  generatedAt: "2026-09-23T00:00:00.000000Z",
  generator: "eyebrow/0.5.3",
  artifacts: [artifactWithFiles, remoteArtifact],
};

const derived = deriveFromLockfile(lockfile);

eq("artifacts are ordered by id, not by scan order", derived.leaves[0].id, "07bf01e342fa4980");

const shuffled = deriveFromLockfile({ ...lockfile, artifacts: [remoteArtifact, artifactWithFiles] });
eq("the root does not depend on the order eyebrow emitted", hex(shuffled.merkleRoot), hex(derived.merkleRoot));
eq("the batch id does not depend on that order either", hex(shuffled.batchId), hex(derived.batchId));

eq(
  "the content hash matches its documented preimage",
  hex(derived.contentHash),
  hex(
    sha(
      "eyebrow-scan-content:v1\n" +
        "eyebrow/0.5.3\n2026-09-23T00:00:00.000000Z\n2\n" +
        `07bf01e342fa4980 ${hex(artifactDigest(remoteArtifact))}\n` +
        `0f04932dbddbdd15 ${"11".repeat(32)}\n`,
    ),
  ),
);

eq(
  "the batch id matches its documented preimage",
  hex(derived.batchId),
  hex(sha(`eyebrow-scan:v1:${hex(derived.contentHash)}`)),
);

eq("receipt count is the artifact count", derived.receiptCount, 2);
eq("batch id is 32 bytes", derived.batchId.length, 32);
eq("merkle root is 32 bytes", derived.merkleRoot.length, 32);

// ---------------------------------------------------------------- tamper

const tampered = structuredClone(lockfile);
tampered.artifacts[0].contentHash = `sha256-${"22".repeat(32)}`;
const tamperedDerived = deriveFromLockfile(tampered);
check(
  "changing one artifact hash changes the merkle root",
  hex(tamperedDerived.merkleRoot) !== hex(derived.merkleRoot),
);
check(
  "changing one artifact hash changes the batch id",
  hex(tamperedDerived.batchId) !== hex(derived.batchId),
);

const dropped = deriveFromLockfile({ ...lockfile, artifacts: [artifactWithFiles] });
check("removing an artifact changes the root", hex(dropped.merkleRoot) !== hex(derived.merkleRoot));
eq("removing an artifact changes the receipt count", dropped.receiptCount, 1);

const retimed = deriveFromLockfile({ ...lockfile, generatedAt: "2026-09-24T00:00:00.000000Z" });
eq("a later scan of identical tooling keeps the root", hex(retimed.merkleRoot), hex(derived.merkleRoot));
check("but takes a different batch id", hex(retimed.batchId) !== hex(derived.batchId));

check(
  "duplicate artifact ids are rejected",
  (() => {
    try {
      canonicalLeaves({ artifacts: [artifactWithFiles, artifactWithFiles] });
      return false;
    } catch {
      return true;
    }
  })(),
);

check(
  "an empty lockfile is refused, since the program requires receipt_count > 0",
  (() => {
    try {
      canonicalLeaves({ artifacts: [] });
      return false;
    } catch {
      return true;
    }
  })(),
);

eq(
  "scanContentHash and batchIdFor compose the way deriveFromLockfile does",
  hex(batchIdFor(scanContentHash(lockfile, canonicalLeaves(lockfile)))),
  hex(derived.batchId),
);

process.stdout.write(`\n${failures === 0 ? "all checks passed" : `${failures} check(s) failed`}\n`);
process.exit(failures === 0 ? 0 : 1);
