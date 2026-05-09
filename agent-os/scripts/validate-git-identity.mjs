#!/usr/bin/env node
import { spawnSync } from "node:child_process";

const projectDomains = new Set(["opencovenant.org", ["covenant", "base.com"].join("")]);
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

const emailDomain = (email) => {
  const at = email.lastIndexOf("@");
  if (at === -1 || at === email.length - 1) {
    return "";
  }
  return email.slice(at + 1).toLowerCase();
};

const isPlatformBot = (name, email) => {
  if (name === "GitHub" && email === "noreply@github.com") {
    return true;
  }
  return (
    name === "dependabot[bot]" &&
    /^[0-9]+\+dependabot\[bot\]@users\.noreply\.github\.com$/i.test(email)
  );
};

const isAllowed = (name, email) => {
  if (isPlatformBot(name, email)) {
    return true;
  }
  return projectDomains.has(emailDomain(email));
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
    if (!isAllowed(commit.authorName, commit.authorEmail)) {
      failures.push(`${ref} ${commit.sha.slice(0, 12)} author identity is not approved`);
    }
    if (!isAllowed(commit.committerName, commit.committerEmail)) {
      failures.push(`${ref} ${commit.sha.slice(0, 12)} committer identity is not approved`);
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
  process.exit(1);
}

console.log(`validate-git-identity: ok (${checked} commits across ${selectedRefs.length} refs)`);
