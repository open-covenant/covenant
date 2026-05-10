#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import {
  approvedGithubCliAccountSummary,
  githubPushAccountViolation,
} from "./identity-policy.mjs";

const remoteName = process.argv[2] || "origin";
const suppliedUrl = process.argv[3] || "";
process.env.COVENANT_IDENTITY_REMOTE = remoteName;

const run = (command, args) =>
  spawnSync(command, args, {
    encoding: "utf8",
  });

const gitText = (args) => {
  const result = run("git", args);
  if (result.status !== 0) {
    return null;
  }
  return result.stdout.trim();
};

const fail = (message) => {
  console.error("validate-github-push-identity: failed");
  console.error(`- ${message}`);
  console.error(`Approved GitHub push accounts: ${approvedGithubCliAccountSummary()}`);
  process.exit(1);
};

const remoteUrl =
  suppliedUrl ||
  gitText(["remote", "get-url", "--push", remoteName]) ||
  gitText(["remote", "get-url", remoteName]);

if (!remoteUrl) {
  fail(`cannot resolve push URL for remote ${remoteName}`);
}

const httpsRepo = /^https:\/\/github\.com\/([^/]+\/[^/.]+)(?:\.git)?$/i.exec(remoteUrl);
const scpRepo = /^git@([^:]+):([^/]+\/[^/]+?)(?:\.git)?$/i.exec(remoteUrl);
const sshRepo = /^ssh:\/\/git@([^/]+)\/([^/]+\/[^/]+?)(?:\.git)?$/i.exec(remoteUrl);

const parseGhAccount = () => {
  const status = run("gh", ["auth", "status", "-h", "github.com"]);
  if (status.error?.code === "ENOENT") {
    fail("gh is unavailable for HTTPS push identity verification");
  }

  const output = `${status.stdout ?? ""}\n${status.stderr ?? ""}`;
  if (status.status !== 0) {
    fail(`gh auth status failed: ${output.trim()}`);
  }

  let pendingAccount = null;
  for (const line of output.split(/\r?\n/)) {
    const accountMatch = /Logged in to github\.com account ([^\s]+) /.exec(line);
    if (accountMatch) {
      pendingAccount = accountMatch[1];
      continue;
    }
    if (pendingAccount && /Active account:\s*true/i.test(line)) {
      return pendingAccount;
    }
    if (/Active account:\s*false/i.test(line)) {
      pendingAccount = null;
    }
  }

  fail("no active GitHub CLI account");
};

const requireApprovedAccount = (account, transport) => {
  const violation = githubPushAccountViolation(account);
  if (violation) {
    fail(`${transport} push account ${violation}`);
  }
};

const requireWritableRepo = (repo) => {
  const view = run("gh", ["repo", "view", repo, "--json", "viewerPermission", "--jq", ".viewerPermission"]);
  if (view.status !== 0) {
    fail(`cannot inspect repository permission for ${repo}: ${(view.stderr || view.stdout).trim()}`);
  }

  const permission = view.stdout.trim();
  if (!new Set(["ADMIN", "MAINTAIN", "WRITE"]).has(permission)) {
    fail(`active GitHub account has ${permission || "unknown"} permission for ${repo}`);
  }
};

if (httpsRepo) {
  const account = parseGhAccount();
  requireApprovedAccount(account, "HTTPS");
  requireWritableRepo(httpsRepo[1]);
  console.log("validate-github-push-identity: ok");
  process.exit(0);
}

const sshMatch = scpRepo || sshRepo;
if (sshMatch) {
  const host = sshMatch[1];
  const probe = run("ssh", ["-T", `git@${host}`]);
  const output = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
  const match = /Hi ([^!]+)! You've successfully authenticated/.exec(output);
  if (!match) {
    fail(`cannot determine SSH push account for ${host}: ${output.trim()}`);
  }

  requireApprovedAccount(match[1], "SSH");
  console.log("validate-github-push-identity: ok");
  process.exit(0);
}

console.log("validate-github-push-identity: skipped (non-GitHub push URL)");
