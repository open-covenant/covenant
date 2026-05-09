#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { approvedIdentitySummary, identityViolation } from "./git-identity-policy.mjs";

const recordSeparator = "\x1e";
const fieldSeparator = "\x1f";

const usage = () => {
  console.error("usage: scripts/validate-git-identity.mjs [--ref <ref>]... [--recent <count>]");
};

const args = process.argv.slice(2);
const refs = [];
let recent = 200;

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--ref") {
    const ref = args[index + 1];
    if (!ref) {
      usage();
      process.exit(2);
    }
    refs.push(ref);
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

const commitExists = (ref) => {
  const result = git(["rev-parse", "--verify", "--quiet", `${ref}^{commit}`]);
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

const scanRef = (ref) => {
  const format = ["%H", "%an", "%ae", "%cn", "%ce", "%s"].join(fieldSeparator) + recordSeparator;
  const result = git(["log", `-${recent}`, `--format=${format}`, ref]);
  if (result.status !== 0) {
    throw new Error(`git log failed for ${ref}: ${(result.stderr || result.stdout).trim()}`);
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

const selectedRefs = [...new Set(refs.length > 0 ? refs : defaultRefs())].filter(commitExists);

if (selectedRefs.length === 0) {
  console.error("validate-git-identity: no commit refs to scan");
  process.exit(1);
}

const failures = [];
let checked = 0;

for (const ref of selectedRefs) {
  for (const commit of scanRef(ref)) {
    checked += 1;
    const authorViolation = identityViolation(commit.authorName, commit.authorEmail);
    if (authorViolation) {
      failures.push(`${ref} ${commit.sha.slice(0, 12)} author identity ${authorViolation}`);
    }
    const committerViolation = identityViolation(commit.committerName, commit.committerEmail);
    if (committerViolation) {
      failures.push(`${ref} ${commit.sha.slice(0, 12)} committer identity ${committerViolation}`);
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

console.log(`validate-git-identity: ok (${checked} commits across ${selectedRefs.length} refs)`);
