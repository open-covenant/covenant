#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import {
  approvedGithubCliAccountSummary,
  githubCliAccountViolation,
} from "./identity-policy.mjs";

const gh = (args) =>
  spawnSync("gh", args, {
    encoding: "utf8",
  });

const status = gh(["auth", "status", "-h", "github.com"]);

if (status.error?.code === "ENOENT") {
  console.log("validate-github-cli-account: skipped (gh unavailable)");
  process.exit(0);
}

const output = `${status.stdout ?? ""}\n${status.stderr ?? ""}`;

if (status.status !== 0) {
  if (/not logged in|no authentication/i.test(output)) {
    console.log("validate-github-cli-account: skipped (gh not authenticated)");
    process.exit(0);
  }
  console.error(`validate-github-cli-account: gh auth status failed: ${output.trim()}`);
  process.exit(1);
}

let pendingAccount = null;
let activeAccount = null;

for (const line of output.split(/\r?\n/)) {
  const accountMatch = /Logged in to github\.com account ([^\s]+) /.exec(line);
  if (accountMatch) {
    pendingAccount = accountMatch[1];
    continue;
  }
  if (pendingAccount && /Active account:\s*true/i.test(line)) {
    activeAccount = pendingAccount;
    break;
  }
  if (/Active account:\s*false/i.test(line)) {
    pendingAccount = null;
  }
}

if (!activeAccount) {
  console.log("validate-github-cli-account: skipped (no active gh account)");
  process.exit(0);
}

const violation = githubCliAccountViolation(activeAccount);
if (violation) {
  console.error("validate-github-cli-account: failed");
  console.error(`- active GitHub CLI account ${violation}`);
  console.error(`Approved GitHub CLI accounts: ${approvedGithubCliAccountSummary()}`);
  process.exit(1);
}

console.log("validate-github-cli-account: ok");
