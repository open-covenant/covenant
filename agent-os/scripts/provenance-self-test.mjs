#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash, generateKeyPairSync } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentRoot = resolve(here, "..");
const repoRoot = resolve(agentRoot, "..");
const provenanceScript = join(agentRoot, "scripts", "provenance.mjs");
const fixture = join(
  repoRoot,
  "docs",
  "provenance",
  "attestations",
  "20ff55e-memory-drift-reports.json",
);

function run(args) {
  return spawnSync(process.execPath, [provenanceScript, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
  });
}

function expectOk(args) {
  const result = run(args);
  if (result.status !== 0) {
    throw new Error(`expected success: ${result.stderr || result.stdout}`);
  }
}

function expectFail(args, expected) {
  const result = run(args);
  if (result.status === 0) {
    throw new Error(`expected failure for ${args.join(" ")}`);
  }
  const output = `${result.stderr}\n${result.stdout}`;
  if (!output.includes(expected)) {
    throw new Error(`expected failure to mention ${expected}; got ${output}`);
  }
}

function stableJson(value) {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(stableJson).join(",")}]`;
  }
  return `{${Object.keys(value)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
    .join(",")}}`;
}

function sha256(input) {
  return createHash("sha256").update(input).digest("hex");
}

function gitCommit(ref) {
  const result = spawnSync("git", ["rev-parse", `${ref}^{commit}`], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(`git rev-parse failed: ${result.stderr}`);
  }
  return result.stdout.trim();
}

function withPayloadDigest(value) {
  const { payloadSha256, ...payload } = value;
  return { ...payload, payloadSha256: sha256(stableJson(payload)) };
}

const tempDir = mkdtempSync(join(tmpdir(), "covenant-provenance-"));
try {
  expectOk(["verify-all"]);

  const original = JSON.parse(readFileSync(fixture, "utf8"));

  const digestTamper = join(tempDir, "digest-tamper.json");
  writeFileSync(
    digestTamper,
    `${JSON.stringify({ ...original, payloadSha256: "0".repeat(64) }, null, 2)}\n`,
  );
  expectFail(["verify", "--file", digestTamper], "payloadSha256 mismatch");

  const identityTamper = join(tempDir, "identity-tamper.json");
  writeFileSync(
    identityTamper,
    `${JSON.stringify(
      { ...original, localPath: ["", "Users", "example", ".ssh", "key"].join("/") },
      null,
      2,
    )}\n`,
  );
  expectFail(["verify", "--file", identityTamper], "forbidden local identity pattern");

  const auditReport = join(tempDir, "audit-report.json");
  writeFileSync(
    auditReport,
    `${JSON.stringify(
      {
        events: 2,
        anchors: 2,
        valid: true,
        root_hash_hex: "a".repeat(64),
        failures: [],
      },
      null,
      2,
    )}\n`,
  );

  const auditRoot = join(tempDir, "audit-root.json");
  expectOk([
    "audit-root",
    "write",
    "--report",
    auditReport,
    "--task",
    "memory-drift-repair",
    "--commit",
    "20ff55e",
    "--out",
    auditRoot,
    "--validation",
    "covenant audit verify=passed",
  ]);
  expectOk(["audit-root", "verify", "--file", auditRoot]);
  expectOk(["verify", "--file", auditRoot]);

  const releaseId = "v0.1.0-alpha.1";
  const releaseCommit = gitCommit("20ff55e");
  const releaseSubject = join(tempDir, "release-subject.json");
  const releaseSubjectEnvelope = {
    schema: "covenant.provenance.release.v1",
    generatedAt: "2026-05-09T00:00:00.000Z",
    subject: {
      kind: "release_bundle",
      repository: "open-covenant/covenant",
      releaseId,
      tag: releaseId,
      commit: releaseCommit,
      artifacts: [
        {
          name: "covenant-source",
          filename: `covenant-${releaseId}.tar.gz`,
          sha256: "c".repeat(64),
          sizeBytes: 123,
        },
      ],
    },
    validation: [
      {
        command: "bash agent-os/scripts/validate.sh --quick",
        status: "passed",
      },
    ],
  };
  writeFileSync(releaseSubject, `${JSON.stringify(releaseSubjectEnvelope, null, 2)}\n`);

  const releaseAuditRoot = join(tempDir, "release-audit-root.json");
  expectOk([
    "audit-root",
    "write",
    "--report",
    auditReport,
    "--release",
    releaseId,
    "--release-subject",
    releaseSubject,
    "--commit",
    releaseCommit,
    "--out",
    releaseAuditRoot,
    "--validation",
    "covenant audit verify=passed",
  ]);
  expectOk(["audit-root", "verify", "--file", releaseAuditRoot]);

  const releaseAuditRootOriginal = JSON.parse(readFileSync(releaseAuditRoot, "utf8"));
  const releaseSubjectTamper = join(tempDir, "release-subject-tamper.json");
  const tamperedReleaseSubject = {
    ...releaseAuditRootOriginal.target.releaseSubject,
    subject: {
      ...releaseAuditRootOriginal.target.releaseSubject.subject,
      releaseId: "v0.1.0-alpha.2",
    },
  };
  writeFileSync(
    releaseSubjectTamper,
    `${JSON.stringify(
      withPayloadDigest({
        ...releaseAuditRootOriginal,
        target: {
          ...releaseAuditRootOriginal.target,
          releaseSubject: tamperedReleaseSubject,
          releaseSubjectSha256: sha256(stableJson(tamperedReleaseSubject)),
        },
      }),
      null,
      2,
    )}\n`,
  );
  expectFail(
    ["audit-root", "verify", "--file", releaseSubjectTamper],
    "release subject metadata mismatch",
  );

  const { privateKey } = generateKeyPairSync("ed25519");
  const signingKey = join(tempDir, "audit-root-signing-key.pem");
  writeFileSync(
    signingKey,
    privateKey.export({ type: "pkcs8", format: "pem" }),
  );

  const signedAuditRoot = join(tempDir, "signed-audit-root.json");
  expectOk([
    "audit-root",
    "write",
    "--report",
    auditReport,
    "--task",
    "memory-drift-repair",
    "--commit",
    "20ff55e",
    "--out",
    signedAuditRoot,
    "--signing-key",
    signingKey,
    "--key-id",
    "covenant-test-root",
    "--validation",
    "covenant audit verify=passed",
  ]);
  expectOk(["audit-root", "verify", "--file", signedAuditRoot]);

  const signedOriginal = JSON.parse(readFileSync(signedAuditRoot, "utf8"));
  const signatureTamper = join(tempDir, "signature-tamper.json");
  writeFileSync(
    signatureTamper,
    `${JSON.stringify(
      withPayloadDigest({
        ...signedOriginal,
        signing: {
          ...signedOriginal.signing,
          signature: Buffer.alloc(64).toString("base64"),
        },
      }),
      null,
      2,
    )}\n`,
  );
  expectFail(["audit-root", "verify", "--file", signatureTamper], "signature verification failed");

  const auditRootOriginal = JSON.parse(readFileSync(auditRoot, "utf8"));
  const unsignedSigningTamper = join(tempDir, "unsigned-signing-tamper.json");
  writeFileSync(
    unsignedSigningTamper,
    `${JSON.stringify(
      withPayloadDigest({
        ...auditRootOriginal,
        signing: {
          ...auditRootOriginal.signing,
          publicKeySpkiBase64: "AA==",
        },
      }),
      null,
      2,
    )}\n`,
  );
  expectFail(
    ["audit-root", "verify", "--file", unsignedSigningTamper],
    "malformed unsigned signing block",
  );

  const auditRootTamper = join(tempDir, "audit-root-tamper.json");
  writeFileSync(
    auditRootTamper,
    `${JSON.stringify({ ...auditRootOriginal, payloadSha256: "0".repeat(64) }, null, 2)}\n`,
  );
  expectFail(["audit-root", "verify", "--file", auditRootTamper], "payloadSha256 mismatch");

  const auditRootMetadataTamper = join(tempDir, "audit-root-metadata-tamper.json");
  const tamperedMetadata = {
    ...auditRootOriginal,
    target: {
      ...auditRootOriginal.target,
      title: "tampered",
    },
  };
  writeFileSync(
    auditRootMetadataTamper,
    `${JSON.stringify(withPayloadDigest(tamperedMetadata), null, 2)}\n`,
  );
  expectFail(
    ["audit-root", "verify", "--file", auditRootMetadataTamper],
    "task target metadata mismatch",
  );

  const invalidAuditReport = join(tempDir, "invalid-audit-report.json");
  writeFileSync(
    invalidAuditReport,
    `${JSON.stringify(
      {
        events: 2,
        anchors: 1,
        valid: false,
        root_hash_hex: "b".repeat(64),
        failures: ["chain length mismatch"],
      },
      null,
      2,
    )}\n`,
  );
  expectFail(
    [
      "audit-root",
      "write",
      "--report",
      invalidAuditReport,
      "--task",
      "memory-drift-repair",
      "--commit",
      "20ff55e",
      "--out",
      join(tempDir, "invalid-audit-root.json"),
    ],
    "audit integrity report must be valid",
  );

  console.log("provenance-self-test: ok");
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}
