#!/usr/bin/env node
import { spawnSync } from "node:child_process";

function usage() {
  console.log(`usage: alpha-release-readiness [--json] [--strict]

Report local alpha release readiness without mutating Git, tags, artifacts, or remotes.

Default mode exits 0 and reports blockers. Use --strict to exit non-zero when blockers exist.`);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trim(),
    stderr: (result.stderr || "").trim(),
  };
}

function textOutput(result) {
  return [result.stdout, result.stderr].filter(Boolean).join("\n");
}

function checkCommand(id, title, command, args, severity = "blocker") {
  const result = run(command, args);
  const displayCommand = command === process.execPath ? "node" : command;
  return {
    id,
    title,
    severity,
    ok: result.status === 0,
    command: [displayCommand, ...args].join(" "),
    output: textOutput(result),
  };
}

function gitStatusCheck() {
  const result = run("git", ["status", "--porcelain"]);
  const dirtyFiles = result.status === 0
    ? result.stdout.split("\n").filter((line) => line.trim() !== "").length
    : null;

  return {
    id: "clean-working-tree",
    title: "Working tree is clean",
    severity: "blocker",
    ok: result.status === 0 && dirtyFiles === 0,
    command: "git status --porcelain",
    output: result.status === 0 ? `${dirtyFiles} dirty path(s)` : textOutput(result),
  };
}

function commitRangeCheck() {
  const origin = run("git", ["rev-parse", "--verify", "origin/main"]);
  const rev = origin.status === 0 ? "origin/main..HEAD" : "HEAD";
  return checkCommand(
    "outgoing-git-identity",
    "Outgoing commits use approved identity metadata",
    process.execPath,
    ["agent-os/scripts/validate-git-identity.mjs", "--rev", rev],
  );
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strict = args.has("--strict");
for (const arg of args) {
  if (!["--json", "--strict"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const checks = [
  gitStatusCheck(),
  checkCommand(
    "commit-rotation",
    "Commit rotation policy validates",
    process.execPath,
    ["agent-os/scripts/validate-commit-rotation.mjs"],
  ),
  checkCommand(
    "current-git-identity",
    "Current Git author and committer are neutral",
    process.execPath,
    ["agent-os/scripts/validate-current-git-identity.mjs"],
  ),
  checkCommand(
    "git-write-access",
    "Git metadata is writable for staging and commits",
    process.execPath,
    ["agent-os/scripts/validate-git-write-access.mjs"],
  ),
  commitRangeCheck(),
  checkCommand(
    "autonomy-records",
    "Autonomy records validate",
    process.execPath,
    ["agent-os/scripts/validate-autonomy.mjs"],
  ),
  checkCommand(
    "autonomy-handoff-toolchain",
    "Autonomy handoff toolchain validates",
    process.execPath,
    ["agent-os/scripts/validate-autonomy-handoff.mjs"],
  ),
  checkCommand(
    "autonomy-review-artifacts",
    "Unsigned autonomy review artifacts validate",
    process.execPath,
    ["agent-os/scripts/validate-autonomy-review-artifacts.mjs"],
  ),
  checkCommand(
    "readme-status-copy",
    "README and status matrix stay aligned",
    process.execPath,
    ["agent-os/scripts/validate-readme-copy.mjs"],
  ),
  checkCommand(
    "live-coverage-metadata",
    "Live coverage metadata validates",
    process.execPath,
    ["agent-os/scripts/validate-live-coverage.mjs"],
  ),
  checkCommand(
    "provenance-attestations",
    "Committed provenance attestations verify",
    process.execPath,
    ["agent-os/scripts/provenance.mjs", "verify-all"],
  ),
  checkCommand(
    "github-push-identity",
    "GitHub push attribution uses an approved project account",
    process.execPath,
    ["agent-os/scripts/validate-github-push-identity.mjs"],
  ),
];

const blockers = checks.filter((check) => !check.ok && check.severity === "blocker");
const report = {
  kind: "alpha_release_readiness",
  generated_at: new Date().toISOString(),
  ready: blockers.length === 0,
  blockers: blockers.map((check) => check.id),
  checks,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`alpha release readiness: ${report.ready ? "ready" : "blocked"}`);
  for (const check of checks) {
    const marker = check.ok ? "ok" : check.severity;
    console.log(`- ${marker}: ${check.title}`);
    if (!check.ok && check.output) {
      console.log(`  ${check.output.split("\n")[0]}`);
    }
  }
}

if (strict && blockers.length > 0) {
  process.exit(1);
}
