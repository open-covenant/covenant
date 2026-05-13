#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  sign as cryptoSign,
  verify as cryptoVerify,
} from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentRoot = resolve(here, "..");
const repoRoot = resolve(agentRoot, "..");
const defaultAttestationDir = join(repoRoot, "docs", "provenance", "attestations");
const defaultAuditRootDir = join(repoRoot, "docs", "provenance", "audit-roots");
const provenanceSchema = "covenant.provenance.v1";
const auditRootSchema = "covenant.audit-root-attestation.v1";
const defaultRepository = "open-covenant/covenant";
const privateEd25519KeyPattern = new RegExp(["id", "ed25519"].join("_"));
const hex64Pattern = /^[0-9a-f]{64}$/;
const base64Pattern = /^[A-Za-z0-9+/]+={0,2}$/;
const keyIdPattern = /^[A-Za-z0-9][A-Za-z0-9_.:-]{1,127}$/;
const repositoryPattern = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const releasePattern = /^[A-Za-z0-9_.:@/-]+$/;

const forbiddenPatterns = [
  /\/Users\//,
  /\/home\/[^/\s"]+/,
  /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/,
  /github[-_]?open[-_]?covenant/i,
  privateEd25519KeyPattern,
];

function usage() {
  console.error(`usage:
  node agent-os/scripts/provenance.mjs write --task <id> --out <path> [--commit <sha>] [--validation "command=passed"]
  node agent-os/scripts/provenance.mjs audit-root write --report <path> --out <path> (--task <id> | --release <id>) [--release-subject <path>] [--commit <sha>] [--previous-root <hex>] [--validation "command=passed"] [--signing-key <pem> --key-id <id>]
  node agent-os/scripts/provenance.mjs audit-root verify --file <path>
  node agent-os/scripts/provenance.mjs verify --file <path>
  node agent-os/scripts/provenance.mjs verify-all [--dir <path>]`);
}

function parseFlags(args) {
  const flags = new Map();
  for (let index = 0; index < args.length; index += 1) {
    const key = args[index];
    if (!key.startsWith("--")) {
      usage();
      process.exit(2);
    }
    const value = args[index + 1];
    if (!value || value.startsWith("--")) {
      usage();
      process.exit(2);
    }
    index += 1;
    const name = key.slice(2);
    const values = flags.get(name) ?? [];
    values.push(value);
    flags.set(name, values);
  }
  return flags;
}

function one(flags, name, fallback = null) {
  const values = flags.get(name);
  if (!values || values.length === 0) return fallback;
  if (values.length > 1) {
    throw new Error(`--${name} may be supplied only once`);
  }
  return values[0];
}

function many(flags, name) {
  return flags.get(name) ?? [];
}

function git(args, options = {}) {
  const result = spawnSync("git", args, {
    cwd: repoRoot,
    encoding: options.encoding ?? "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    const stderr = Buffer.isBuffer(result.stderr)
      ? result.stderr.toString("utf8")
      : result.stderr;
    throw new Error(`git ${args.join(" ")} failed: ${stderr.trim()}`);
  }
  return result.stdout;
}

function gitText(commit, path) {
  return git(["show", `${commit}:${path}`]);
}

function gitBytes(commit, path) {
  return git(["show", `${commit}:${path}`], { encoding: "buffer" });
}

function sha256(input) {
  return createHash("sha256").update(input).digest("hex");
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

function parseJsonl(text, label) {
  return text
    .split(/\r?\n/)
    .filter((line) => line.trim().length > 0)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${label}:${index + 1}: invalid JSON: ${error.message}`);
      }
    });
}

function changedFiles(commit) {
  return git(["diff-tree", "--root", "--no-commit-id", "--name-only", "-r", commit])
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .sort();
}

function blobOid(commit, path) {
  const output = git(["ls-tree", commit, "--", path]).trim();
  const match = output.match(/^\d+\s+blob\s+([0-9a-f]{40,64})\t/);
  if (!match) {
    throw new Error(`no blob for ${path} at ${commit}`);
  }
  return match[1];
}

function fileEvidence(commit, path) {
  const bytes = gitBytes(commit, path);
  return {
    path,
    gitBlob: blobOid(commit, path),
    sha256: sha256(bytes),
  };
}

function fullCommit(commit) {
  return git(["rev-parse", `${commit}^{commit}`]).trim();
}

function taskPath(taskId) {
  return `agent-os/autonomy/tasks/${taskId}.json`;
}

function taskSnapshot(commit, taskId) {
  return JSON.parse(gitText(commit, taskPath(taskId)));
}

function taskEvents(commit, taskId) {
  const events = parseJsonl(gitText(commit, "agent-os/autonomy/events.jsonl"), "events.jsonl");
  return events.filter((event) => event.taskId === taskId);
}

function parseValidation(value) {
  const split = value.lastIndexOf("=");
  if (split <= 0) {
    throw new Error(`validation must be "command=passed|failed|skipped": ${value}`);
  }
  const command = value.slice(0, split).trim();
  const status = value.slice(split + 1).trim();
  if (!["passed", "failed", "skipped"].includes(status)) {
    throw new Error(`unsupported validation status: ${status}`);
  }
  if (command.length === 0) {
    throw new Error("validation command must not be empty");
  }
  return { command, status };
}

function assertHex64(value, label) {
  if (typeof value !== "string" || !hex64Pattern.test(value)) {
    throw new Error(`${label} must be a lowercase 64-character hex string`);
  }
}

function assertRepository(value) {
  if (typeof value !== "string" || !repositoryPattern.test(value)) {
    throw new Error(`repository must be owner/name: ${value}`);
  }
}

function assertReleaseId(value) {
  if (typeof value !== "string" || !releasePattern.test(value)) {
    throw new Error(`release id contains unsupported characters: ${value}`);
  }
}

function assertIsoTimestamp(value, label) {
  if (typeof value !== "string" || Number.isNaN(Date.parse(value))) {
    throw new Error(`${label} must be an ISO timestamp`);
  }
}

function assertNoPrivateStrings(value, label) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  for (const pattern of forbiddenPatterns) {
    if (pattern.test(text)) {
      throw new Error(`${label} contains a forbidden local identity pattern: ${pattern}`);
    }
  }
}

function stripDigest(attestation) {
  const { payloadSha256, ...payload } = attestation;
  return payload;
}

function signaturePayload(attestation) {
  const payload = stripDigest(attestation);
  return {
    ...payload,
    signing: {
      ...payload.signing,
      signature: null,
    },
  };
}

function assertKeyId(value) {
  if (typeof value !== "string" || !keyIdPattern.test(value)) {
    throw new Error(`key id contains unsupported characters: ${value}`);
  }
}

function base64Bytes(value, label) {
  if (typeof value !== "string" || !base64Pattern.test(value)) {
    throw new Error(`${label} must be base64`);
  }
  const bytes = Buffer.from(value, "base64");
  if (bytes.length === 0 || bytes.toString("base64") !== value) {
    throw new Error(`${label} must be canonical base64`);
  }
  return bytes;
}

function parseAuditReport(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label}: expected audit integrity report object`);
  }

  const events = value.events;
  const anchors = value.anchors;
  const rootHashHex = value.root_hash_hex ?? value.rootHashHex;
  const failures = value.failures ?? [];

  if (!Number.isSafeInteger(events) || events < 0) {
    throw new Error(`${label}: events must be a non-negative integer`);
  }
  if (!Number.isSafeInteger(anchors) || anchors < 0) {
    throw new Error(`${label}: anchors must be a non-negative integer`);
  }
  if (value.valid !== true) {
    throw new Error(`${label}: audit integrity report must be valid`);
  }
  if (events !== anchors) {
    throw new Error(`${label}: event and anchor counts must match`);
  }
  assertHex64(rootHashHex, `${label}: root_hash_hex`);
  if (!Array.isArray(failures) || failures.some((item) => typeof item !== "string")) {
    throw new Error(`${label}: failures must be an array of strings`);
  }
  if (failures.length > 0) {
    throw new Error(`${label}: valid audit report must not contain failures`);
  }

  return {
    events,
    anchors,
    valid: true,
    root_hash_hex: rootHashHex,
    failures,
  };
}

function attest(taskId, commit, validationValues) {
  const resolvedCommit = fullCommit(commit);
  const task = taskSnapshot(resolvedCommit, taskId);
  const events = taskEvents(resolvedCommit, taskId);
  const files = changedFiles(resolvedCommit).map((path) => fileEvidence(resolvedCommit, path));

  const payload = {
    schema: provenanceSchema,
    generatedAt: new Date().toISOString(),
    subject: {
      kind: "git_commit",
      commit: resolvedCommit,
      files,
    },
    task: {
      id: task.id,
      title: task.title,
      state: task.state,
      priority: task.priority,
      ownerRole: task.ownerRole,
      snapshotPath: taskPath(taskId),
      snapshotSha256: sha256(stableJson(task)),
    },
    workflow: {
      eventLogPath: "agent-os/autonomy/events.jsonl",
      events,
      eventsSha256: sha256(stableJson(events)),
    },
    verification: validationValues.map(parseValidation),
    claims: [
      "The subject commit and file blobs are read from Git object data.",
      "The task snapshot and transition events are read from the subject commit.",
      "Validation entries are recorded evidence supplied by the operator or automation that produced this attestation.",
    ],
    limits: [
      "This envelope is not a public transparency-log entry.",
      "This envelope does not claim a public release-signing identity.",
      "Commit signature verification is outside this schema.",
    ],
  };
  assertNoPrivateStrings(payload, "attestation");
  return { ...payload, payloadSha256: sha256(stableJson(payload)) };
}

function validateReleaseSubject(value, context) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${context}: release subject must be an object`);
  }
  if (value.schema !== "covenant.provenance.release.v1") {
    throw new Error(`${context}: unsupported release subject schema`);
  }
  assertIsoTimestamp(value.generatedAt, `${context}: generatedAt`);
  const subject = value.subject;
  if (!subject || typeof subject !== "object" || Array.isArray(subject)) {
    throw new Error(`${context}: subject must be an object`);
  }
  if (subject.kind !== "release_bundle") {
    throw new Error(`${context}: subject.kind must be release_bundle`);
  }
  assertRepository(subject.repository);
  assertReleaseId(subject.releaseId);
  const commit = fullCommit(subject.commit);
  if (commit !== subject.commit) {
    throw new Error(`${context}: subject commit is not canonical`);
  }
  if (!Array.isArray(subject.artifacts) || subject.artifacts.length === 0) {
    throw new Error(`${context}: subject.artifacts must be non-empty`);
  }
  for (const [index, artifact] of subject.artifacts.entries()) {
    if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
      throw new Error(`${context}: artifact ${index} must be an object`);
    }
    if (typeof artifact.name !== "string" || artifact.name.trim() === "") {
      throw new Error(`${context}: artifact ${index} name must be non-empty`);
    }
    assertHex64(artifact.sha256, `${context}: artifact ${index} sha256`);
    if (!Number.isSafeInteger(artifact.sizeBytes) || artifact.sizeBytes < 0) {
      throw new Error(`${context}: artifact ${index} sizeBytes must be a non-negative integer`);
    }
  }
  if (!Array.isArray(value.validation) || value.validation.length === 0) {
    throw new Error(`${context}: validation evidence must be non-empty`);
  }
  for (const [index, item] of value.validation.entries()) {
    if (
      !item ||
      typeof item.command !== "string" ||
      item.command.trim() === "" ||
      !["passed", "failed", "skipped"].includes(item.status)
    ) {
      throw new Error(`${context}: validation ${index} is invalid`);
    }
  }
  return value;
}

function readReleaseSubject(path) {
  return validateReleaseSubject(
    JSON.parse(readFileSync(resolve(repoRoot, path), "utf8")),
    "--release-subject",
  );
}

function auditRootTarget(commit, taskId, releaseId, releaseSubjectPath, repository) {
  if ((taskId && releaseId) || (!taskId && !releaseId)) {
    throw new Error("audit-root attestation requires exactly one of --task or --release");
  }
  if (releaseSubjectPath && !releaseId) {
    throw new Error("--release-subject requires --release");
  }
  if (taskId) {
    const task = taskSnapshot(commit, taskId);
    return {
      kind: "task",
      id: task.id,
      title: task.title,
      state: task.state,
      priority: task.priority,
      snapshotPath: taskPath(taskId),
      snapshotSha256: sha256(stableJson(task)),
    };
  }
  return releaseRootTarget(commit, releaseId, releaseSubjectPath, repository);
}

function releaseRootTarget(commit, releaseId, releaseSubjectPath, repository) {
  assertReleaseId(releaseId);
  const target = {
    kind: "release",
    id: releaseId,
  };
  if (!releaseSubjectPath) return target;
  const releaseSubject = readReleaseSubject(releaseSubjectPath);
  const subject = releaseSubject.subject;
  if (
    subject.releaseId !== releaseId ||
    subject.repository !== repository ||
    subject.commit !== commit
  ) {
    throw new Error("--release-subject metadata does not match --release, --repository, and --commit");
  }
  return {
    ...target,
    releaseSubject,
    releaseSubjectSha256: sha256(stableJson(releaseSubject)),
  };
}

function auditReportFromPayload(auditRoot) {
  return {
    events: auditRoot.events,
    anchors: auditRoot.anchors,
    valid: auditRoot.valid,
    root_hash_hex: auditRoot.rootHashHex,
    failures: auditRoot.failures,
  };
}

function assertReleaseSubjectBinding(target, commit, repository, attestationPath) {
  assertReleaseId(target.id);
  if (target.releaseSubject === undefined) {
    return;
  }
  const subjectEnvelope = validateReleaseSubject(
    target.releaseSubject,
    `${attestationPath}: target.releaseSubject`,
  );
  const subject = subjectEnvelope.subject;
  if (
    subject.releaseId !== target.id ||
    subject.repository !== repository ||
    subject.commit !== commit
  ) {
    throw new Error(`${attestationPath}: release subject metadata mismatch`);
  }
  if (target.releaseSubjectSha256 !== sha256(stableJson(subjectEnvelope))) {
    throw new Error(`${attestationPath}: release subject digest mismatch`);
  }
}

function signAuditRootPayload(payload, signingKeyPath, keyId) {
  assertKeyId(keyId);
  const privateKey = createPrivateKey(readFileSync(resolve(repoRoot, signingKeyPath)));
  const publicKeyDer = createPublicKey(privateKey).export({ type: "spki", format: "der" });
  const signing = {
    status: "signed",
    keyId,
    algorithm: "ed25519",
    publicKeySpkiSha256: sha256(publicKeyDer),
    publicKeySpkiBase64: Buffer.from(publicKeyDer).toString("base64"),
    signature: null,
  };
  const signable = { ...payload, signing };
  const signature = cryptoSign(null, Buffer.from(stableJson(signable)), privateKey).toString(
    "base64",
  );
  return {
    ...payload,
    signing: {
      ...signing,
      signature,
    },
  };
}

function verifyAuditRootSignature(attestation, attestationPath) {
  const signing = attestation.signing;
  if (signing?.status === "unsigned") {
    if (
      signing.keyId !== null ||
      signing.algorithm !== null ||
      signing.signature !== null ||
      signing.publicKeySpkiSha256 != null ||
      signing.publicKeySpkiBase64 != null
    ) {
      throw new Error(`${attestationPath}: malformed unsigned signing block`);
    }
    return;
  }

  if (signing?.status !== "signed") {
    throw new Error(`${attestationPath}: unsupported audit-root signing status`);
  }
  assertKeyId(signing.keyId);
  if (signing.algorithm !== "ed25519") {
    throw new Error(`${attestationPath}: unsupported audit-root signing algorithm`);
  }

  const publicKeyDer = base64Bytes(
    signing.publicKeySpkiBase64,
    `${attestationPath}: signing.publicKeySpkiBase64`,
  );
  if (signing.publicKeySpkiSha256 !== sha256(publicKeyDer)) {
    throw new Error(`${attestationPath}: public key digest mismatch`);
  }

  const publicKey = createPublicKey({ key: publicKeyDer, format: "der", type: "spki" });
  const signature = base64Bytes(signing.signature, `${attestationPath}: signing.signature`);
  const ok = cryptoVerify(
    null,
    Buffer.from(stableJson(signaturePayload(attestation))),
    publicKey,
    signature,
  );
  if (!ok) {
    throw new Error(`${attestationPath}: signature verification failed`);
  }
}

function auditRootAttest(options) {
  const resolvedCommit = fullCommit(options.commit);
  const reportPath = resolve(repoRoot, options.report);
  const report = parseAuditReport(JSON.parse(readFileSync(reportPath, "utf8")), reportPath);
  const repository = options.repository ?? defaultRepository;
  assertRepository(repository);

  const previousRootHashHex = options.previousRoot ?? null;
  if (previousRootHashHex !== null) {
    assertHex64(previousRootHashHex, "--previous-root");
  }
  if (Boolean(options.signingKey) !== Boolean(options.keyId)) {
    throw new Error("audit-root signing requires both --signing-key and --key-id");
  }

  let payload = {
    schema: auditRootSchema,
    generatedAt: new Date().toISOString(),
    repository,
    subject: {
      kind: "git_commit",
      commit: resolvedCommit,
    },
    target: auditRootTarget(
      resolvedCommit,
      options.taskId,
      options.releaseId,
      options.releaseSubject,
      repository,
    ),
    auditRoot: {
      events: report.events,
      anchors: report.anchors,
      valid: report.valid,
      rootHashHex: report.root_hash_hex,
      failures: report.failures,
      reportSha256: sha256(stableJson(report)),
    },
    previous: {
      rootHashHex: previousRootHashHex,
    },
    signing: {
      status: "unsigned",
      keyId: null,
      algorithm: null,
      signature: null,
    },
    verification: options.validationValues.map(parseValidation),
    claims: [
      "The audit root was produced from a valid covenant audit verify report.",
      "The subject commit is resolved from the local Git object database.",
      "Task targets are bound to the task snapshot stored in the subject commit.",
      "Release targets may bind an explicit release subject digest before project key custody exists.",
    ],
    limits: [
      "This audit-root attestation is not a signature.",
      "This audit-root attestation is not a transparency-log entry.",
      "This audit-root attestation does not claim immutable retention.",
    ],
  };
  if (options.signingKey) {
    payload = {
      ...payload,
      limits: [
        "This audit-root attestation is a detached signature, not a transparency-log entry.",
        "Embedded public key verification proves payload integrity for that key, not project key custody.",
        "This audit-root attestation does not claim immutable retention.",
      ],
    };
    payload = signAuditRootPayload(payload, options.signingKey, options.keyId);
  }
  assertNoPrivateStrings(payload, "audit root attestation");
  return { ...payload, payloadSha256: sha256(stableJson(payload)) };
}

function verifyProvenance(attestationPath) {
  const attestation = JSON.parse(readFileSync(attestationPath, "utf8"));
  if (attestation.schema !== provenanceSchema) {
    throw new Error(`${attestationPath}: unsupported schema ${attestation.schema}`);
  }
  assertNoPrivateStrings(attestation, attestationPath);

  const expectedDigest = sha256(stableJson(stripDigest(attestation)));
  if (attestation.payloadSha256 !== expectedDigest) {
    throw new Error(`${attestationPath}: payloadSha256 mismatch`);
  }

  const commit = fullCommit(attestation.subject.commit);
  if (commit !== attestation.subject.commit) {
    throw new Error(`${attestationPath}: subject commit is not canonical`);
  }

  for (const file of attestation.subject.files) {
    const evidence = fileEvidence(commit, file.path);
    if (file.gitBlob !== evidence.gitBlob || file.sha256 !== evidence.sha256) {
      throw new Error(`${attestationPath}: file evidence mismatch for ${file.path}`);
    }
  }

  const expectedChangedFiles = changedFiles(commit);
  const attestedPaths = attestation.subject.files.map((file) => file.path).sort();
  if (stableJson(expectedChangedFiles) !== stableJson(attestedPaths)) {
    throw new Error(`${attestationPath}: subject file list does not match commit diff`);
  }

  const task = taskSnapshot(commit, attestation.task.id);
  if (attestation.task.snapshotSha256 !== sha256(stableJson(task))) {
    throw new Error(`${attestationPath}: task snapshot digest mismatch`);
  }

  const events = taskEvents(commit, attestation.task.id);
  if (attestation.workflow.eventsSha256 !== sha256(stableJson(events))) {
    throw new Error(`${attestationPath}: workflow event digest mismatch`);
  }
  if (stableJson(attestation.workflow.events) !== stableJson(events)) {
    throw new Error(`${attestationPath}: embedded workflow events do not match subject commit`);
  }

  for (const item of attestation.verification) {
    if (!item.command || !["passed", "failed", "skipped"].includes(item.status)) {
      throw new Error(`${attestationPath}: invalid verification item`);
    }
  }
}

function verifyAuditRoot(attestationPath) {
  const attestation = JSON.parse(readFileSync(attestationPath, "utf8"));
  if (attestation.schema !== auditRootSchema) {
    throw new Error(`${attestationPath}: unsupported schema ${attestation.schema}`);
  }
  assertNoPrivateStrings(attestation, attestationPath);

  const expectedDigest = sha256(stableJson(stripDigest(attestation)));
  if (attestation.payloadSha256 !== expectedDigest) {
    throw new Error(`${attestationPath}: payloadSha256 mismatch`);
  }

  assertIsoTimestamp(attestation.generatedAt, `${attestationPath}: generatedAt`);
  assertRepository(attestation.repository);

  const commit = fullCommit(attestation.subject.commit);
  if (commit !== attestation.subject.commit || attestation.subject.kind !== "git_commit") {
    throw new Error(`${attestationPath}: subject commit is not canonical`);
  }

  const rootReport = parseAuditReport(
    auditReportFromPayload(attestation.auditRoot),
    `${attestationPath}: auditRoot`,
  );
  if (attestation.auditRoot.reportSha256 !== sha256(stableJson(rootReport))) {
    throw new Error(`${attestationPath}: audit report digest mismatch`);
  }

  if (attestation.previous?.rootHashHex !== null) {
    assertHex64(attestation.previous?.rootHashHex, `${attestationPath}: previous.rootHashHex`);
  }

  if (attestation.target?.kind === "task") {
    const task = taskSnapshot(commit, attestation.target.id);
    if (
      attestation.target.title !== task.title ||
      attestation.target.state !== task.state ||
      attestation.target.priority !== task.priority
    ) {
      throw new Error(`${attestationPath}: task target metadata mismatch`);
    }
    if (attestation.target.snapshotPath !== taskPath(attestation.target.id)) {
      throw new Error(`${attestationPath}: task snapshot path mismatch`);
    }
    if (attestation.target.snapshotSha256 !== sha256(stableJson(task))) {
      throw new Error(`${attestationPath}: task snapshot digest mismatch`);
    }
  } else if (attestation.target?.kind === "release") {
    assertReleaseSubjectBinding(attestation.target, commit, attestation.repository, attestationPath);
  } else {
    throw new Error(`${attestationPath}: unsupported target kind`);
  }

  verifyAuditRootSignature(attestation, attestationPath);

  for (const item of attestation.verification) {
    if (!item.command || !["passed", "failed", "skipped"].includes(item.status)) {
      throw new Error(`${attestationPath}: invalid verification item`);
    }
  }
}

function verifyAny(attestationPath) {
  const attestation = JSON.parse(readFileSync(attestationPath, "utf8"));
  if (attestation.schema === provenanceSchema) {
    verifyProvenance(attestationPath);
    return;
  }
  if (attestation.schema === auditRootSchema) {
    verifyAuditRoot(attestationPath);
    return;
  }
  throw new Error(`${attestationPath}: unsupported schema ${attestation.schema}`);
}

function jsonFiles(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .flatMap((entry) => {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) return jsonFiles(path);
      if (entry.isFile() && entry.name.endsWith(".json")) return [path];
      return [];
    })
    .sort();
}

function writeFile(path, data) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(data, null, 2)}\n`);
}

try {
  const [command, ...args] = process.argv.slice(2);
  if (command === "write") {
    const flags = parseFlags(args);
    const taskId = one(flags, "task");
    const out = one(flags, "out");
    const commit = one(flags, "commit", "HEAD");
    if (!taskId || !out) {
      usage();
      process.exit(2);
    }
    const attestation = attest(taskId, commit, many(flags, "validation"));
    writeFile(resolve(repoRoot, out), attestation);
    console.log(`wrote ${relative(repoRoot, resolve(repoRoot, out))}`);
  } else if (command === "audit-root") {
    const [subcommand, ...subargs] = args;
    const flags = parseFlags(subargs);
    if (subcommand === "write") {
      const out = one(flags, "out");
      const report = one(flags, "report");
      if (!out || !report) {
        usage();
        process.exit(2);
      }
      const attestation = auditRootAttest({
        report,
        commit: one(flags, "commit", "HEAD"),
        taskId: one(flags, "task"),
        releaseId: one(flags, "release"),
        releaseSubject: one(flags, "release-subject"),
        repository: one(flags, "repository", defaultRepository),
        previousRoot: one(flags, "previous-root"),
        validationValues: many(flags, "validation"),
        signingKey: one(flags, "signing-key"),
        keyId: one(flags, "key-id"),
      });
      writeFile(resolve(repoRoot, out), attestation);
      console.log(`wrote ${relative(repoRoot, resolve(repoRoot, out))}`);
    } else if (subcommand === "verify") {
      const file = one(flags, "file");
      if (!file) {
        usage();
        process.exit(2);
      }
      verifyAuditRoot(resolve(repoRoot, file));
      console.log(`provenance: ok (${file})`);
    } else {
      usage();
      process.exit(2);
    }
  } else if (command === "verify") {
    const flags = parseFlags(args);
    const file = one(flags, "file");
    if (!file) {
      usage();
      process.exit(2);
    }
    verifyAny(resolve(repoRoot, file));
    console.log(`provenance: ok (${file})`);
  } else if (command === "verify-all") {
    const flags = parseFlags(args);
    const explicitDir = one(flags, "dir");
    const files = explicitDir
      ? jsonFiles(resolve(repoRoot, explicitDir))
      : [defaultAttestationDir, defaultAuditRootDir].flatMap(jsonFiles);
    for (const file of files) verifyAny(file);
    console.log(`provenance: ok (${files.length} attestation${files.length === 1 ? "" : "s"})`);
  } else {
    usage();
    process.exit(2);
  }
} catch (error) {
  console.error(`provenance: ${error.message}`);
  process.exit(1);
}
