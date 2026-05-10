#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const tasksDir = join(root, "autonomy", "tasks");

function usage() {
  console.log(`usage: validate-autonomy-review-artifacts [task-id]

Run the read-only autonomy review artifact toolchain validation.

Checks artifact generation, artifact verification, and expected digest tamper rejection without writing files or Git metadata.`);
}

function run(args, input = null) {
  const result = spawnSync(process.execPath, args, {
    input,
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trimEnd(),
    stderr: (result.stderr || "").trimEnd(),
  };
}

function fail(errors) {
  console.error("validate-autonomy-review-artifacts: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

function parseJson(label, text, errors) {
  try {
    return JSON.parse(text);
  } catch (error) {
    errors.push(`${label}: invalid JSON: ${error.message}`);
    return null;
  }
}

function findDefaultTask() {
  const tasks = readdirSync(tasksDir)
    .filter((file) => file.endsWith(".json"))
    .map((file) => JSON.parse(readFileSync(join(tasksDir, file), "utf8")))
    .filter((task) => task.state === "integrated")
    .sort((left, right) => left.id.localeCompare(right.id));

  return tasks.find((task) => task.id === "autonomy-review-artifact-verifier")
    ?? tasks.find((task) => task.id === "autonomy-review-artifact-scaffold")
    ?? tasks[0]
    ?? null;
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
if (args.length > 1) {
  usage();
  process.exit(2);
}

const taskId = args[0] ?? findDefaultTask()?.id ?? "";
if (!/^[a-z0-9][a-z0-9-]*$/.test(taskId)) {
  fail(["task id must be lowercase kebab-case or at least one integrated task must exist"]);
}

const errors = [];

const artifactResult = run(["agent-os/scripts/autonomy-review-artifact.mjs", taskId, "--json"]);
if (artifactResult.status !== 0) {
  errors.push(`autonomy-review-artifact failed: ${artifactResult.stderr || artifactResult.stdout}`);
}
const artifact = parseJson("review artifact", artifactResult.stdout, errors);
if (artifact?.kind !== "autonomy_review_artifact") {
  errors.push("review artifact kind mismatch");
}
if (artifact?.task?.id !== taskId) {
  errors.push("review artifact task id mismatch");
}
if (artifact?.signing?.status !== "unsigned") {
  errors.push("review artifact must be unsigned until signing support exists");
}

const verifyResult = run(
  ["agent-os/scripts/autonomy-verify-review-artifact.mjs", "--stdin", "--json"],
  artifactResult.stdout,
);
if (verifyResult.status !== 0) {
  errors.push(`review artifact verifier rejected generated artifact: ${verifyResult.stderr || verifyResult.stdout}`);
}
const verification = parseJson("review artifact verification", verifyResult.stdout, errors);
if (verification?.valid !== true) {
  errors.push("review artifact verification should be valid");
}

if (artifact) {
  const tampered = {
    ...artifact,
    digests: {
      ...artifact.digests,
      task_sha256: "0".repeat(64),
    },
  };
  const tamperedResult = run(
    ["agent-os/scripts/autonomy-verify-review-artifact.mjs", "--stdin", "--json"],
    JSON.stringify(tampered),
  );
  if (tamperedResult.status === 0) {
    errors.push("review artifact verifier accepted a tampered task digest");
  } else {
    const tamperedReport = parseJson("tamper verification", tamperedResult.stdout, errors);
    const tamperDetected = tamperedReport?.errors?.some((error) =>
      error.includes("task_sha256"),
    );
    if (!tamperDetected) {
      errors.push("review artifact verifier did not report task_sha256 tampering");
    }
  }
}

if (errors.length > 0) {
  fail(errors);
}

console.log(`validate-autonomy-review-artifacts: ok (${taskId})`);
