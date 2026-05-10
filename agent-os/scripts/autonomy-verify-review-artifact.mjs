#!/usr/bin/env node
import { createHash, createPublicKey, verify } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function usage() {
  console.log(`usage: autonomy-verify-review-artifact (--stdin | <artifact.json>) [--json] [--trusted-public-key-spki-base64 value]

Verify an autonomy review artifact against the local task record and transition log without writing files or Git metadata.

Signed artifacts require an explicit trusted ed25519 SPKI public key in base64 DER form.`);
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function safePath(path) {
  return typeof path === "string"
    && path.length > 0
    && !path.startsWith("/")
    && !path.includes("..")
    && !path.startsWith(".git/");
}

function repoPath(path) {
  if (!safePath(path)) return null;
  return resolve(repoRoot, path);
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function scanForbidden(value, errors, pointer = "$") {
  const forbidden = [
    [new RegExp(`/${"Users"}/[^\\s"')\\]]+`), "machine-local home path"],
    [new RegExp(`Co-${"Authored-By"}:`, "i"), "commit attribution trailer"],
    [new RegExp(`${"Generated"} with`, "i"), "AI generation attribution"],
  ];

  if (typeof value === "string") {
    for (const [pattern, label] of forbidden) {
      if (pattern.test(value)) {
        errors.push(`${pointer} contains forbidden ${label}`);
      }
    }
    return;
  }

  if (Array.isArray(value)) {
    value.forEach((item, index) => scanForbidden(item, errors, `${pointer}[${index}]`));
    return;
  }

  if (value && typeof value === "object") {
    for (const [key, item] of Object.entries(value)) {
      scanForbidden(item, errors, `${pointer}.${key}`);
    }
  }
}

function readEvents(path, taskId, errors) {
  if (!existsSync(path)) {
    errors.push("source.events_path does not exist");
    return { events: [], lines: [] };
  }

  const lines = readFileSync(path, "utf8").replace(/\r\n/g, "\n").split("\n").filter(Boolean);
  const selectedEvents = [];
  const selectedLines = [];
  for (const line of lines) {
    let event;
    try {
      event = JSON.parse(line);
    } catch (error) {
      errors.push(`source.events_path contains invalid JSONL: ${error.message}`);
      continue;
    }
    if (event.taskId === taskId) {
      selectedEvents.push(event);
      selectedLines.push(line);
    }
  }
  return { events: selectedEvents, lines: selectedLines };
}

function taskEnvelope(task) {
  return {
    id: task.id,
    title: task.title,
    state: task.state,
    priority: task.priority,
    owner_role: task.ownerRole,
    updated: task.updated,
    scope: task.scope,
    gates: task.gates,
    verification: task.verification,
    human_escalation: task.humanEscalation,
    expected_failure_modes: task.expectedFailureModes,
  };
}

function signingPayload(artifact) {
  return Buffer.from(JSON.stringify({
    ...artifact,
    signing: {
      ...artifact.signing,
      signature: "",
    },
  }));
}

function publicKeyFromSpkiBase64(value, errors) {
  if (!value) return null;
  let der;
  try {
    der = Buffer.from(value, "base64");
  } catch (error) {
    errors.push(`trusted public key is not base64: ${error.message}`);
    return null;
  }
  if (der.length === 0) {
    errors.push("trusted public key is empty");
    return null;
  }
  try {
    return {
      der,
      key: createPublicKey({
        key: der,
        format: "der",
        type: "spki",
      }),
    };
  } catch (error) {
    errors.push(`trusted public key is not a valid SPKI key: ${error.message}`);
    return null;
  }
}

function validateSigning(artifact, trustedPublicKey, errors) {
  const signing = artifact?.signing;
  if (signing?.status === "unsigned") {
    return;
  }

  if (signing?.status !== "signed") {
    errors.push("signing.status must be unsigned or signed");
    return;
  }
  if (signing.schema !== "covenant.autonomy-review-signature.v1") {
    errors.push("signing.schema must be covenant.autonomy-review-signature.v1");
  }
  if (signing.algorithm !== "ed25519") {
    errors.push("signing.algorithm must be ed25519");
  }
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(signing.key_id || "")) {
    errors.push("signing.key_id must be a stable lowercase key id");
  }
  if (!/^\d{4}-\d{2}-\d{2}T/.test(signing.signed_at || "")) {
    errors.push("signing.signed_at must be ISO-like");
  }
  if (!/^[a-f0-9]{64}$/.test(signing.public_key_spki_sha256 || "")) {
    errors.push("signing.public_key_spki_sha256 must be lowercase sha256 hex");
  }
  if (typeof signing.signature !== "string" || signing.signature.length === 0) {
    errors.push("signing.signature is required for signed artifacts");
  }
  if (!safePath(signing.custody?.policy || "")) {
    errors.push("signing.custody.policy must be a safe repository-relative path");
  }
  if (signing.custody?.human_approval_required !== true) {
    errors.push("signing.custody.human_approval_required must be true");
  }
  if (!trustedPublicKey) {
    errors.push("signed artifacts require --trusted-public-key-spki-base64");
    return;
  }

  const trustedDigest = sha256(trustedPublicKey.der);
  if (signing.public_key_spki_sha256 !== trustedDigest) {
    errors.push("signing.public_key_spki_sha256 does not match trusted public key");
    return;
  }

  let signature;
  try {
    signature = Buffer.from(signing.signature, "base64");
  } catch (error) {
    errors.push(`signing.signature is not base64: ${error.message}`);
    return;
  }
  if (signature.length === 0) {
    errors.push("signing.signature is empty");
    return;
  }
  if (!verify(null, signingPayload(artifact), trustedPublicKey.key, signature)) {
    errors.push("signing.signature does not verify");
  }
}

function validateArtifact(artifact, trustedPublicKey = null) {
  const errors = [];

  if (artifact?.kind !== "autonomy_review_artifact") {
    errors.push("kind must be autonomy_review_artifact");
  }
  if (artifact?.schema_version !== 1) {
    errors.push("schema_version must be 1");
  }
  if (!/^\d{4}-\d{2}-\d{2}T/.test(artifact?.generated_at || "")) {
    errors.push("generated_at must be ISO-like");
  }
  if (!safePath(artifact?.source?.task_path)) {
    errors.push("source.task_path must be a safe repository-relative path");
  }
  if (!safePath(artifact?.source?.events_path)) {
    errors.push("source.events_path must be a safe repository-relative path");
  }
  validateSigning(artifact, trustedPublicKey, errors);

  const taskId = artifact?.task?.id;
  if (!/^[a-z0-9][a-z0-9-]*$/.test(taskId || "")) {
    errors.push("task.id must be lowercase kebab-case");
  }

  const taskPath = repoPath(artifact?.source?.task_path);
  const eventsPath = repoPath(artifact?.source?.events_path);
  if (!taskPath || !eventsPath || !taskId) {
    scanForbidden(artifact, errors);
    return errors;
  }

  if (!existsSync(taskPath)) {
    errors.push("source.task_path does not exist");
  } else {
    let taskText;
    let task;
    try {
      taskText = readFileSync(taskPath, "utf8").replace(/\r\n/g, "\n");
      task = JSON.parse(taskText);
    } catch (error) {
      errors.push(`source.task_path cannot be parsed: ${error.message}`);
    }

    if (task) {
      if (task.id !== taskId) {
        errors.push("task.id does not match source task record");
      }
      if (artifact?.digests?.task_sha256 !== sha256(taskText)) {
        errors.push("digests.task_sha256 does not match source task record");
      }
      if (!sameJson(artifact.task, taskEnvelope(task))) {
        errors.push("task metadata does not match source task record");
      }
    }
  }

  const { events, lines } = readEvents(eventsPath, taskId, errors);
  const expectedEventDigest = sha256(lines.join("\n") + (lines.length > 0 ? "\n" : ""));
  if (artifact?.digests?.event_subset_sha256 !== expectedEventDigest) {
    errors.push("digests.event_subset_sha256 does not match transition events");
  }
  if (artifact?.review_trace?.event_count !== events.length) {
    errors.push("review_trace.event_count does not match transition events");
  }
  if (artifact?.review_trace?.first_event_at !== (events[0]?.timestamp ?? null)) {
    errors.push("review_trace.first_event_at does not match transition events");
  }
  if (artifact?.review_trace?.last_event_at !== (events.at(-1)?.timestamp ?? null)) {
    errors.push("review_trace.last_event_at does not match transition events");
  }
  if (!sameJson(artifact?.review_trace?.events, events)) {
    errors.push("review_trace.events does not match transition events");
  }

  scanForbidden(artifact, errors);
  return errors;
}

const argv = process.argv.slice(2);
if (argv.includes("--help") || argv.includes("-h")) {
  usage();
  process.exit(0);
}

let asJson = false;
let fromStdin = false;
let file = "";
let trustedPublicKeySpkiBase64 = "";
for (let index = 0; index < argv.length; index += 1) {
  const arg = argv[index];
  if (arg === "--json") {
    asJson = true;
    continue;
  }
  if (arg === "--stdin") {
    fromStdin = true;
    continue;
  }
  if (arg === "--trusted-public-key-spki-base64") {
    trustedPublicKeySpkiBase64 = argv[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (!file) {
    file = arg;
    continue;
  }
  usage();
  process.exit(2);
}

if ((fromStdin && file) || (!fromStdin && !file)) {
  usage();
  process.exit(2);
}

const keyErrors = [];
const trustedPublicKey = publicKeyFromSpkiBase64(trustedPublicKeySpkiBase64, keyErrors);
let artifact;
try {
  const input = fromStdin ? readFileSync(0, "utf8") : readFileSync(file, "utf8");
  artifact = JSON.parse(input);
} catch (error) {
  console.error("autonomy-verify-review-artifact: failed");
  console.error(`- cannot read artifact: ${error.message}`);
  process.exit(1);
}

const errors = [...keyErrors, ...validateArtifact(artifact, trustedPublicKey)];
const report = {
  kind: "autonomy_review_artifact_verification",
  valid: errors.length === 0,
  errors,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else if (errors.length === 0) {
  console.log("autonomy-verify-review-artifact: ok");
} else {
  console.error("autonomy-verify-review-artifact: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
}

if (errors.length > 0) {
  process.exit(1);
}
