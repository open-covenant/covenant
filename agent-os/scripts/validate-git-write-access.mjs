#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { closeSync, openSync, unlinkSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";

function usage() {
  console.log(`usage: validate-git-write-access

Verify that the local Git metadata directory is writable enough to stage and commit.

The probe creates and removes one transient file in the Git metadata directory. It does not mutate refs, commits, branches, tags, remotes, or the working tree.`);
}

function fail(message, code = 1) {
  console.error("validate-git-write-access: failed");
  console.error(`- ${message}`);
  process.exit(code);
}

function git(args) {
  return execFileSync("git", args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
if (args.length > 0) {
  usage();
  process.exit(2);
}

let root;
let gitDir;
try {
  root = git(["rev-parse", "--show-toplevel"]);
  const rawGitDir = git(["rev-parse", "--git-dir"]);
  gitDir = isAbsolute(rawGitDir) ? rawGitDir : resolve(root, rawGitDir);
} catch {
  fail("not a Git repository", 2);
}

const probePath = resolve(gitDir, `covenant-write-access-${process.pid}.tmp`);
let fd;

try {
  fd = openSync(probePath, "wx", 0o600);
} catch (error) {
  const code = error?.code ? ` (${error.code})` : "";
  fail(`cannot create a transient probe file in the Git metadata directory${code}`);
}

try {
  closeSync(fd);
  unlinkSync(probePath);
} catch (error) {
  const code = error?.code ? ` (${error.code})` : "";
  fail(`cannot clean up the transient Git metadata probe${code}`);
}

console.log("validate-git-write-access: ok");
