#!/usr/bin/env node
import { spawnSync } from "node:child_process";

function usage() {
  console.log(`usage: autonomy-preflight [--json] [--strict]

Report local autonomy environment readiness without mutating Git, remotes, or task state.

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

function checkCommand(id, title, command, args, tags = []) {
  const result = run(command, args);
  const displayCommand = command === process.execPath ? "node" : command;
  return {
    id,
    title,
    tags,
    ok: result.status === 0,
    command: [displayCommand, ...args].join(" "),
    output: textOutput(result),
  };
}

function cleanTreeCheck() {
  const result = run("git", ["status", "--porcelain"]);
  const dirtyFiles = result.status === 0
    ? result.stdout.split("\n").filter((line) => line.trim() !== "").length
    : null;

  return {
    id: "clean-working-tree",
    title: "Working tree is clean",
    tags: ["commit"],
    ok: result.status === 0 && dirtyFiles === 0,
    command: "git status --porcelain",
    output: result.status === 0 ? `${dirtyFiles} dirty path(s)` : textOutput(result),
  };
}

function outgoingIdentityCheck() {
  const origin = run("git", ["rev-parse", "--verify", "origin/main"]);
  const rev = origin.status === 0 ? "origin/main..HEAD" : "HEAD";
  return checkCommand(
    "outgoing-git-identity",
    "Outgoing commits use approved identity metadata",
    process.execPath,
    ["agent-os/scripts/validate-git-identity.mjs", "--rev", rev],
    ["commit", "push"],
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
  cleanTreeCheck(),
  checkCommand(
    "current-git-identity",
    "Current Git author and committer are neutral",
    process.execPath,
    ["agent-os/scripts/validate-current-git-identity.mjs"],
    ["commit", "push"],
  ),
  checkCommand(
    "git-write-access",
    "Git metadata is writable for staging and commits",
    process.execPath,
    ["agent-os/scripts/validate-git-write-access.mjs"],
    ["commit"],
  ),
  outgoingIdentityCheck(),
  checkCommand(
    "autonomy-records",
    "Autonomy records validate",
    process.execPath,
    ["agent-os/scripts/validate-autonomy.mjs"],
    ["commit"],
  ),
  checkCommand(
    "github-push-identity",
    "GitHub push attribution uses an approved project account",
    process.execPath,
    ["agent-os/scripts/validate-github-push-identity.mjs"],
    ["push"],
  ),
];

const blockers = checks.filter((check) => !check.ok);
const commitBlockers = blockers
  .filter((check) => check.tags.includes("commit"))
  .map((check) => check.id);
const pushBlockers = blockers
  .filter((check) => check.tags.includes("push"))
  .map((check) => check.id);

const report = {
  kind: "autonomy_preflight",
  generated_at: new Date().toISOString(),
  commit_ready: commitBlockers.length === 0,
  push_ready: pushBlockers.length === 0,
  blockers: blockers.map((check) => check.id),
  commit_blockers: commitBlockers,
  push_blockers: pushBlockers,
  checks,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`autonomy preflight: ${blockers.length === 0 ? "ready" : "blocked"}`);
  console.log(`commit: ${report.commit_ready ? "ready" : "blocked"}`);
  console.log(`push: ${report.push_ready ? "ready" : "blocked"}`);
  for (const check of checks) {
    const marker = check.ok ? "ok" : "blocker";
    console.log(`- ${marker}: ${check.title}`);
    if (!check.ok && check.output) {
      console.log(`  ${check.output.split("\n")[0]}`);
    }
  }
}

if (strict && blockers.length > 0) {
  process.exit(1);
}
