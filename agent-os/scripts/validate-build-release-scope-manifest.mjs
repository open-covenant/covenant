#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");

function runGenerator(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/build-release-scope-manifest.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function runInspector(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/release-scope-manifest.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function canonicalize(value) {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalize).join(",")}]`;
  }
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalize(value[k])}`).join(",")}}`;
  }
  throw new Error(`unsupported value type: ${typeof value}`);
}

function sha256Utf8(value) {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

const fixtureRoot = mkdtempSync(join(tmpdir(), "covenant-release-scope-build-"));
const taskDir = join(fixtureRoot, "tasks");
mkdirSync(taskDir);

const tasks = [
  {
    id: "alpha-task",
    state: "integrated",
    title: "First task",
    nested: { b: 1, a: 2, c: [3, 2, 1] },
  },
  {
    id: "bravo-task",
    state: "integrated",
    title: "Second task",
  },
  {
    id: "charlie-task",
    state: "blocked",
    title: "Third task",
    notes: "blocked on human decision",
  },
];

for (const task of tasks) {
  writeJson(join(taskDir, `${task.id}.json`), task);
}

const baseArgs = [
  "--task-dir", taskDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--previous-tag", "v0.0.9",
];

const errors = [];
const fail = (message) => errors.push(message);

const firstRun = runGenerator(baseArgs);
if (firstRun.status !== 0) {
  fail(`first generator run failed: ${firstRun.stderr || firstRun.stdout}`);
}

let firstManifest;
try {
  firstManifest = JSON.parse(firstRun.stdout);
} catch (error) {
  fail(`first generator output is not JSON: ${error.message}`);
  firstManifest = null;
}

if (firstManifest) {
  if (firstManifest.schema !== "covenant.release-scope.v1") {
    fail("manifest schema must be covenant.release-scope.v1");
  }
  if (firstManifest.release.tag !== "v0.1.0" || firstManifest.release.previousTag !== "v0.0.9") {
    fail("manifest must echo release tag and previousTag");
  }
  if (firstManifest.scope.task_count !== 3) {
    fail(`task_count must equal record count (3); got ${firstManifest.scope.task_count}`);
  }
  if (firstManifest.scope.task_state_distribution.integrated !== 2
    || firstManifest.scope.task_state_distribution.blocked !== 1) {
    fail("task_state_distribution must reflect 2 integrated + 1 blocked");
  }
  if (typeof firstManifest.scope.task_set_sha256 !== "string"
    || !/^[0-9a-f]{64}$/.test(firstManifest.scope.task_set_sha256)) {
    fail("task_set_sha256 must be 64-char lowercase hex");
  }

  const sortedTasks = [...tasks].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const expectedDigest = sha256Utf8(sortedTasks.map(canonicalize).join("\n"));
  if (firstManifest.scope.task_set_sha256 !== expectedDigest) {
    fail(`task_set_sha256 mismatch with locally computed digest; expected ${expectedDigest}, got ${firstManifest.scope.task_set_sha256}`);
  }
}

const secondRun = runGenerator(baseArgs);
if (secondRun.status !== 0) {
  fail(`second generator run failed: ${secondRun.stderr || secondRun.stdout}`);
}

let secondManifest;
try {
  secondManifest = JSON.parse(secondRun.stdout);
} catch (error) {
  fail(`second generator output is not JSON: ${error.message}`);
  secondManifest = null;
}

if (firstManifest && secondManifest) {
  if (firstManifest.scope.task_set_sha256 !== secondManifest.scope.task_set_sha256) {
    fail("task_set_sha256 is not reproducible across runs");
  }
  if (firstManifest.scope.task_count !== secondManifest.scope.task_count) {
    fail("task_count is not reproducible across runs");
  }
}

const tampered = { ...tasks[0], title: "Tampered title" };
writeJson(join(taskDir, "alpha-task.json"), tampered);
const tamperedRun = runGenerator(baseArgs);
if (tamperedRun.status !== 0) {
  fail(`tampered-corpus generator run failed: ${tamperedRun.stderr || tamperedRun.stdout}`);
}
let tamperedManifest;
try {
  tamperedManifest = JSON.parse(tamperedRun.stdout);
} catch (error) {
  fail(`tampered manifest output is not JSON: ${error.message}`);
  tamperedManifest = null;
}
if (tamperedManifest && firstManifest && tamperedManifest.scope.task_set_sha256 === firstManifest.scope.task_set_sha256) {
  fail("byte-level change in a record must change task_set_sha256");
}

writeJson(join(taskDir, "alpha-task.json"), tasks[0]);

const inspectorPath = join(fixtureRoot, "manifest.json");
const cleanArgs = [...baseArgs, "--output", inspectorPath];
const cleanRun = runGenerator(cleanArgs);
if (cleanRun.status !== 0) {
  fail(`--output run failed: ${cleanRun.stderr || cleanRun.stdout}`);
}
const inspectorRun = runInspector(["--path", inspectorPath, "--json"]);
if (inspectorRun.status !== 0) {
  fail(`inspector rejected generator output: ${inspectorRun.stderr || inspectorRun.stdout}`);
}
let inspectorReport;
try {
  inspectorReport = JSON.parse(inspectorRun.stdout);
} catch (error) {
  fail(`inspector output is not JSON: ${error.message}`);
  inspectorReport = null;
}
if (inspectorReport && inspectorReport.ok !== true) {
  fail(`inspector reported ok:false on generator output: ${JSON.stringify(inspectorReport.errors)}`);
}

const firstReleaseArgs = [
  "--task-dir", taskDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--first-release",
];
const firstReleaseRun = runGenerator(firstReleaseArgs);
if (firstReleaseRun.status !== 0) {
  fail(`--first-release run failed: ${firstReleaseRun.stderr || firstReleaseRun.stdout}`);
}
let firstReleaseManifest;
try {
  firstReleaseManifest = JSON.parse(firstReleaseRun.stdout);
} catch (error) {
  fail(`--first-release output is not JSON: ${error.message}`);
  firstReleaseManifest = null;
}
if (firstReleaseManifest && firstReleaseManifest.release.previousTag !== null) {
  fail("--first-release must produce release.previousTag = null");
}

const conflictRun = runGenerator([...baseArgs, "--first-release"]);
if (conflictRun.status === 0) {
  fail("--first-release and --previous-tag together must fail");
}

const missingPreviousRun = runGenerator([
  "--task-dir", taskDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
]);
if (missingPreviousRun.status === 0) {
  fail("missing --previous-tag / --first-release must fail");
}

const badCommitRun = runGenerator([
  "--task-dir", taskDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "not-a-sha",
  "--first-release",
]);
if (badCommitRun.status === 0) {
  fail("non-hex --commit must fail");
}

const badRepoRun = runGenerator([
  "--task-dir", taskDir,
  "--repository", "bad slug",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--first-release",
]);
if (badRepoRun.status === 0) {
  fail("invalid --repository must fail");
}

const corruptDir = join(fixtureRoot, "corrupt");
mkdirSync(corruptDir);
writeJson(join(corruptDir, "good.json"), { id: "good-task", state: "integrated" });
writeFileSync(join(corruptDir, "broken.json"), "{ not json");
const corruptRun = runGenerator([
  "--task-dir", corruptDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--first-release",
]);
if (corruptRun.status === 0) {
  fail("malformed JSON record in --task-dir must fail");
}

const missingFieldDir = join(fixtureRoot, "missing-field");
mkdirSync(missingFieldDir);
writeJson(join(missingFieldDir, "no-state.json"), { id: "no-state-task" });
const missingFieldRun = runGenerator([
  "--task-dir", missingFieldDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--first-release",
]);
if (missingFieldRun.status === 0) {
  fail("task record without a state must fail");
}

const unknownStateDir = join(fixtureRoot, "unknown-state");
mkdirSync(unknownStateDir);
writeJson(join(unknownStateDir, "bad-state.json"), { id: "bad", state: "mystery" });
const unknownStateRun = runGenerator([
  "--task-dir", unknownStateDir,
  "--repository", "open-covenant/covenant",
  "--release-id", "v0.1.0",
  "--tag", "v0.1.0",
  "--commit", "0123456789abcdef0123456789abcdef01234567",
  "--first-release",
]);
if (unknownStateRun.status === 0) {
  fail("task record with unknown state must fail");
}

const usageRun = runGenerator([]);
if (usageRun.status === 0) {
  fail("missing required flags must fail");
}

const unknownFlagRun = runGenerator([...baseArgs, "--mystery"]);
if (unknownFlagRun.status === 0) {
  fail("unknown flag must fail");
}

const onDiskManifest = JSON.parse(readFileSync(inspectorPath, "utf8"));
const reEncoded = sha256Utf8([...tasks].sort((a, b) => (a.id < b.id ? -1 : 1)).map(canonicalize).join("\n"));
if (onDiskManifest.scope.task_set_sha256 !== reEncoded) {
  fail("on-disk manifest digest differs from independent recomputation");
}

if (errors.length > 0) {
  console.error("validate-build-release-scope-manifest: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-build-release-scope-manifest: ok");
