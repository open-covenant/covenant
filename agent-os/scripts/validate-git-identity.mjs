#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { approvedIdentitySummary, identityViolation } from "./git-identity-policy.mjs";

const recordSeparator = "\x1e";
const fieldSeparator = "\x1f";

const usage = () => {
  console.error(
    "usage: scripts/validate-git-identity.mjs [--ref <ref-or-range>]... [--rev <ref-or-range>]... [--recent <count>]"
  );
};

const args = process.argv.slice(2);
const revs = [];
let recent = 14;
let recentExplicit = false;

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--rev" || arg === "--ref") {
    const rev = args[index + 1];
    if (!rev) {
      usage();
      process.exit(2);
    }
    revs.push(rev);
    index += 1;
    continue;
  }
  if (arg === "--recent") {
    const parsed = Number.parseInt(args[index + 1] ?? "", 10);
    if (!Number.isInteger(parsed) || parsed < 1) {
      usage();
      process.exit(2);
    }
    recent = parsed;
    recentExplicit = true;
    index += 1;
    continue;
  }
  usage();
  process.exit(2);
}

const git = (args, options = {}) =>
  spawnSync("git", args, {
    encoding: "utf8",
    ...options
  });

const gitText = (args) => {
  const result = git(args);
  if (result.status !== 0) {
    return null;
  }
  return result.stdout.trim();
};

const commitExists = (rev) => {
  const result = git(["rev-parse", "--verify", "--quiet", `${rev}^{commit}`]);
  return result.status === 0;
};

const defaultRefs = () => {
  const selected = ["HEAD"];
  const upstream = gitText(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]);
  if (upstream) {
    selected.push(upstream);
  }
  return selected;
};

const isZeroSha = (value) => /^0{40}$/.test(value);

const splitRange = (rev) => {
  if (rev.includes("...")) {
    const [from, to] = rev.split("...");
    return { from, to };
  }
  const [from, to] = rev.split("..");
  return { from, to };
};

const revisionExists = (rev) => {
  if (!rev.includes("..")) {
    return commitExists(rev);
  }
  const { from, to } = splitRange(rev);
  if (!to || !commitExists(to)) {
    return false;
  }
  if (from && !isZeroSha(from) && !commitExists(from)) {
    return false;
  }
  return true;
};

const scanRev = (rev) => {
  const format = ["%H", "%an", "%ae", "%cn", "%ce", "%s"].join(fieldSeparator) + recordSeparator;
  const logArgs = ["log", `--format=${format}`];
  if (recentExplicit || (!rev.includes("..") && recent)) {
    logArgs.push(`-${recent}`);
  }
  logArgs.push(rev);
  const result = git(logArgs);
  if (result.status !== 0) {
    throw new Error(`git log failed for ${rev}: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout
    .split(recordSeparator)
    .map((record) => record.trim())
    .filter(Boolean)
    .map((record) => {
      const [sha, authorName, authorEmail, committerName, committerEmail, subject] =
        record.split(fieldSeparator);
      return {
        sha,
        authorName,
        authorEmail,
        committerName,
        committerEmail,
        subject
      };
    });
};

const selectedRevs = [...new Set(revs.length > 0 ? revs : defaultRefs())].filter(revisionExists);

if (selectedRevs.length === 0) {
  console.error("validate-git-identity: no commit refs to scan");
  process.exit(1);
}

const failures = [];
let checked = 0;

for (const rev of selectedRevs) {
  for (const commit of scanRev(rev)) {
    checked += 1;
    const authorViolation = identityViolation(commit.authorName, commit.authorEmail);
    if (authorViolation) {
      failures.push(`${rev} ${commit.sha.slice(0, 12)} author identity ${authorViolation}`);
    }
    const committerViolation = identityViolation(commit.committerName, commit.committerEmail);
    if (committerViolation) {
      failures.push(`${rev} ${commit.sha.slice(0, 12)} committer identity ${committerViolation}`);
    }
  }
}

if (failures.length > 0) {
  console.error("validate-git-identity: failed");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  console.error(
    "Inspect the commit with: git show -s --format='%an <%ae> | %cn <%ce>' <sha>"
  );
  console.error(`Approved project identities: ${approvedIdentitySummary()}`);
  process.exit(1);
}

console.log(`validate-git-identity: ok (${checked} commits across ${selectedRevs.length} revs)`);
