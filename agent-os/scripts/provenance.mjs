#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
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
const schema = "covenant.provenance.v1";
const privateEd25519KeyPattern = new RegExp(["id", "ed25519"].join("_"));

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

function attest(taskId, commit, validationValues) {
  const resolvedCommit = fullCommit(commit);
  const task = taskSnapshot(resolvedCommit, taskId);
  const events = taskEvents(resolvedCommit, taskId);
  const files = changedFiles(resolvedCommit).map((path) => fileEvidence(resolvedCommit, path));

  const payload = {
    schema,
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
      "This alpha envelope is not a public transparency-log entry.",
      "This alpha envelope does not claim a public release-signing identity.",
      "Commit signature verification is outside this schema.",
    ],
  };
  assertNoPrivateStrings(payload, "attestation");
  return { ...payload, payloadSha256: sha256(stableJson(payload)) };
}

function verify(attestationPath) {
  const attestation = JSON.parse(readFileSync(attestationPath, "utf8"));
  if (attestation.schema !== schema) {
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
  const flags = parseFlags(args);
  if (command === "write") {
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
  } else if (command === "verify") {
    const file = one(flags, "file");
    if (!file) {
      usage();
      process.exit(2);
    }
    verify(resolve(repoRoot, file));
    console.log(`provenance: ok (${file})`);
  } else if (command === "verify-all") {
    const dir = resolve(repoRoot, one(flags, "dir", defaultAttestationDir));
    const files = jsonFiles(dir);
    for (const file of files) verify(file);
    console.log(`provenance: ok (${files.length} attestation${files.length === 1 ? "" : "s"})`);
  } else {
    usage();
    process.exit(2);
  }
} catch (error) {
  console.error(`provenance: ${error.message}`);
  process.exit(1);
}
