#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import process from "node:process";

const schema = "covenant.alpha-release-evidence.v1";

function runGit(args, opts = {}) {
  return execFileSync("git", args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...opts,
  }).trim();
}

function runNode(args, cwd) {
  return execFileSync(process.execPath, args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

function usage() {
  console.log(`usage: alpha-release-evidence [--json]

Print read-only alpha release evidence for the current Git commit.

This command does not tag, push, publish, sign, or execute validation gates.`);
}

function fail(message) {
  console.error(`alpha-release-evidence: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const args = { json: false };
  for (const a of argv) {
    if (a === "--help" || a === "-h") {
      usage();
      process.exit(0);
    }
    if (a === "--json") {
      args.json = true;
      continue;
    }
    throw new Error(`unknown arg: ${a}`);
  }
  return args;
}

function safeLines(text) {
  return text
    .split("\n")
    .map((l) => l.trimEnd())
    .filter((l) => l.length > 0);
}

function readiness(root) {
  const report = JSON.parse(runNode(["agent-os/scripts/alpha-release-readiness.mjs", "--json"], root));
  return {
    kind: report.kind,
    generated_at: report.generated_at,
    ready: report.ready,
    blockers: report.blockers,
    checks: report.checks.map((check) => ({
      id: check.id,
      title: check.title,
      severity: check.severity,
      ok: check.ok,
      command: check.command,
    })),
  };
}

function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    fail(error.message);
  }

  let root;
  try {
    root = runGit(["rev-parse", "--show-toplevel"]);
  } catch {
    fail("not a git repository");
  }

  const sha = runGit(["rev-parse", "HEAD"], { cwd: root });
  const shortSha = runGit(["rev-parse", "--short", "HEAD"], { cwd: root });
  const branch = runGit(["branch", "--show-current"], { cwd: root }) || "(detached)";
  const dirty = safeLines(runGit(["status", "--porcelain"], { cwd: root })).length;

  const now = new Date().toISOString();
  const readinessReport = readiness(root);

  const commands = [
    "node agent-os/scripts/alpha-release-readiness.mjs --strict",
    "bash agent-os/scripts/validate.sh --quick",
    "pnpm --dir landing build",
    "node agent-os/scripts/model-availability.mjs",
    "cd agent-os && cargo test --workspace --exclude covenant-settlement-program -- --ignored live_",
  ];

  const notes = [
    "Read-only helper: does not tag, push, or publish.",
    "Human approval required before tagging or publishing release artifacts.",
    "Accepted release evidence requires alpha readiness to report ready=true.",
    "Live tests are opt-in and may require external services (e.g. Ollama, Linux runsc).",
  ];

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          schema,
          kind: "alpha_release_evidence",
          generated_at: now,
          commit: sha,
          commit_short: shortSha,
          branch,
          dirty_files: dirty,
          readiness: readinessReport,
          commands,
          notes,
        },
        null,
        2,
      ),
    );
    return;
  }

  console.log("alpha release evidence (read-only)");
  console.log(`generated: ${now}`);
  console.log(`commit: ${sha}`);
  console.log(`branch: ${branch}`);
  console.log(`working tree: ${dirty === 0 ? "clean" : `dirty (${dirty} path(s))`}`);
  console.log(`readiness: ${readinessReport.ready ? "ready" : `blocked (${readinessReport.blockers.join(", ")})`}`);
  console.log("\ncommands:");
  for (const cmd of commands) {
    console.log(`  ${cmd}`);
  }
  console.log("\nnotes:");
  for (const note of notes) {
    console.log(`  - ${note}`);
  }
}

main();
