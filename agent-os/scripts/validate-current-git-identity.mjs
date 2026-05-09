#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import {
  approvedCurrentIdentitySummary,
  currentIdentityViolation,
  parseGitIdent,
} from "./git-identity-policy.mjs";

const git = (args) =>
  spawnSync("git", args, {
    encoding: "utf8",
  });

const gitIdent = (kind) => {
  const result = git(["var", `GIT_${kind}_IDENT`]);
  if (result.status !== 0) {
    const detail = (result.stderr || result.stdout).trim();
    throw new Error(`git var GIT_${kind}_IDENT failed: ${detail}`);
  }
  return parseGitIdent(result.stdout);
};

const failures = [];

for (const kind of ["AUTHOR", "COMMITTER"]) {
  let identity;
  try {
    identity = gitIdent(kind);
  } catch (error) {
    failures.push(`${kind.toLowerCase()} identity is unavailable: ${error.message}`);
    continue;
  }

  const violation = currentIdentityViolation(identity.name, identity.email);
  if (violation) {
    failures.push(`${kind.toLowerCase()} identity ${violation}`);
  }
}

if (failures.length > 0) {
  console.error("validate-current-git-identity: failed");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  console.error(`Approved current identity: ${approvedCurrentIdentitySummary()}`);
  process.exit(1);
}

console.log("validate-current-git-identity: ok");
