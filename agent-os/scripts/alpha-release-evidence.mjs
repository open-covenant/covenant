#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import process from "node:process";

function runGit(args, opts = {}) {
  return execFileSync("git", args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...opts,
  }).trim();
}

function usage() {
  console.log("usage: alpha-release-evidence [--json]");
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

  const commands = [
    "bash agent-os/scripts/validate.sh --quick",
    "pnpm --dir landing build",
    "node agent-os/scripts/probe-ollama.mjs",
    "cd agent-os && cargo test --workspace --exclude covenant-settlement-program -- --ignored live_",
  ];

  const notes = [
    "Read-only helper: does not tag, push, or publish.",
    "Human approval required before tagging or publishing release artifacts.",
    "Live tests are opt-in and may require external services (e.g. Ollama, Linux runsc).",
  ];

  if (args.json) {
    console.log(
      JSON.stringify(
        {
          kind: "alpha_release_evidence",
          generated_at: now,
          commit: sha,
          commit_short: shortSha,
          branch,
          dirty_files: dirty,
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
