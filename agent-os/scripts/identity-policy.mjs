import { spawnSync } from "node:child_process";
import { loadCommitRotation } from "./identity-rotation.mjs";

const commitRotation = loadCommitRotation();
const activeProjectIdentity = commitRotation.currentGitIdentity;

const approvedCurrentPeople = new Map([[activeProjectIdentity.name, activeProjectIdentity.email]]);

const approvedPeople = new Map(
  commitRotation.approvedGitIdentities.map((identity) => [identity.name, identity.email]),
);

const approvedGithubCliAccounts = new Set(commitRotation.approvedGithubAccounts);

const remoteIdentityPolicySummary = "contributors already present on the configured remote";
const remoteGithubPolicySummary = "GitHub contributors on the configured remote";

const blockedIdentityPatterns = [
  {
    label: "absolute workstation path",
    pattern: new RegExp(`/${"Users"}/[^\\s"'<>]+`),
  },
  {
    label: "absolute home path",
    pattern: new RegExp(`/${"home"}/[^\\s"'<>]+`),
  },
];

const blockedIdentityViolation = (value) => {
  for (const { label, pattern } of blockedIdentityPatterns) {
    if (pattern.test(value)) {
      return `contains blocked ${label}`;
    }
  }
  return null;
};

const run = (command, args) =>
  spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

const gitText = (args) => {
  const result = run("git", args);
  if (result.status !== 0) {
    return null;
  }
  return result.stdout.trim();
};

const configuredRemoteName = () => process.env.COVENANT_IDENTITY_REMOTE || commitRotation.remote;

const parseGithubRepo = (remoteUrl) => {
  if (!remoteUrl) {
    return null;
  }

  const httpsRepo = /^https:\/\/github\.com\/([^/]+\/[^/]+?)(?:\.git)?$/i.exec(remoteUrl);
  if (httpsRepo) {
    return httpsRepo[1];
  }

  const scpRepo = /^git@[^:]+:([^/]+\/[^/]+?)(?:\.git)?$/i.exec(remoteUrl);
  if (scpRepo) {
    return scpRepo[1];
  }

  const sshRepo = /^ssh:\/\/git@[^/]+\/([^/]+\/[^/]+?)(?:\.git)?$/i.exec(remoteUrl);
  return sshRepo?.[1] ?? null;
};

const configuredGithubRepo = () => {
  const remoteName = configuredRemoteName();
  return parseGithubRepo(
    gitText(["remote", "get-url", "--push", remoteName]) ||
      gitText(["remote", "get-url", remoteName]),
  );
};

const identityKey = (name, email) => `${name.trim()}\0${email.trim()}`;

let remoteContributorIdentityKeysCache = null;
const remoteContributorIdentityKeys = () => {
  if (remoteContributorIdentityKeysCache) {
    return remoteContributorIdentityKeysCache;
  }

  const identities = new Set();
  const remoteName = configuredRemoteName();
  const refs = (
    gitText(["for-each-ref", "--format=%(refname:short)", `refs/remotes/${remoteName}`]) || ""
  )
    .split(/\r?\n/)
    .map((ref) => ref.trim())
    .filter((ref) => ref && ref !== `${remoteName}/HEAD`);

  if (refs.length === 0) {
    remoteContributorIdentityKeysCache = identities;
    return identities;
  }

  const result = run("git", ["log", "--format=%an%x1f%ae%x1f%cn%x1f%ce%x1e", ...refs]);
  if (result.status !== 0) {
    remoteContributorIdentityKeysCache = identities;
    return identities;
  }

  for (const record of result.stdout.split("\x1e")) {
    if (!record.trim()) {
      continue;
    }
    const [authorName, authorEmail, committerName, committerEmail] = record.split("\x1f");
    for (const [name, email] of [
      [authorName, authorEmail],
      [committerName, committerEmail],
    ]) {
      if (!name || !email || blockedIdentityViolation(`${name} ${email}`)) {
        continue;
      }
      identities.add(identityKey(name, email));
    }
  }

  remoteContributorIdentityKeysCache = identities;
  return identities;
};

let remoteGithubAccountsCache = null;
const remoteGithubAccounts = () => {
  if (remoteGithubAccountsCache) {
    return remoteGithubAccountsCache;
  }

  const accounts = new Set();
  const repo = configuredGithubRepo();
  if (!repo) {
    remoteGithubAccountsCache = accounts;
    return accounts;
  }

  const result = run("gh", [
    "api",
    "--paginate",
    `repos/${repo}/contributors`,
    "--jq",
    ".[].login",
  ]);
  if (result.status !== 0) {
    remoteGithubAccountsCache = accounts;
    return accounts;
  }

  for (const account of result.stdout.split(/\r?\n/).map((line) => line.trim())) {
    if (account && !blockedIdentityViolation(account)) {
      accounts.add(account);
    }
  }

  remoteGithubAccountsCache = accounts;
  return accounts;
};

export function isPlatformBot(name, email) {
  if (name === "GitHub" && email === "noreply@github.com") {
    return true;
  }
  if (
    name === "github-actions[bot]" &&
    /^41898282\+github-actions\[bot\]@users\.noreply\.github\.com$/i.test(email)
  ) {
    return true;
  }
  return (
    name === "dependabot[bot]" &&
    /^[0-9]+\+dependabot\[bot\]@users\.noreply\.github\.com$/i.test(email)
  );
}

export function identityViolation(name, email) {
  if (!name || !email) {
    return "name and email must both be present";
  }

  const blocked = blockedIdentityViolation(`${name} ${email}`);
  if (blocked) {
    return blocked;
  }

  if (isPlatformBot(name, email)) {
    return null;
  }

  if (approvedPeople.get(name) === email) {
    return null;
  }

  if (
    commitRotation.allowConfiguredRemoteGitIdentities &&
    remoteContributorIdentityKeys().has(identityKey(name, email))
  ) {
    return null;
  }

  return "not an approved project identity";
}

export function githubCliAccountViolation(account) {
  if (!account) {
    return "active account is unavailable";
  }

  const blocked = blockedIdentityViolation(account);
  if (blocked) {
    return blocked;
  }

  if (approvedGithubCliAccounts.has(account)) {
    return null;
  }

  if (
    commitRotation.allowConfiguredRemoteGithubContributors &&
    remoteGithubAccounts().has(account)
  ) {
    return null;
  }

  return "not an approved GitHub CLI account";
}

export function githubPushAccountViolation(account) {
  const violation = githubCliAccountViolation(account);
  if (violation === "not an approved GitHub CLI account") {
    return "not an approved GitHub push account";
  }
  return violation;
}

export function currentIdentityViolation(name, email) {
  const violation = identityViolation(name, email);
  if (violation) {
    return violation;
  }

  if (approvedCurrentPeople.get(name) === email) {
    return null;
  }

  return "not the active autonomous project identity";
}

export function activeProjectGitIdentity() {
  return { ...activeProjectIdentity };
}

export function approvedIdentitySummary() {
  const summaries = [...approvedPeople.entries()].map(([name, email]) => `${name} <${email}>`);
  if (commitRotation.allowConfiguredRemoteGitIdentities) {
    summaries.push(remoteIdentityPolicySummary);
  }
  return summaries.join(", ");
}

export function approvedCurrentIdentitySummary() {
  return [...approvedCurrentPeople.entries()]
    .map(([name, email]) => `${name} <${email}>`)
    .join(", ");
}

export function approvedGithubCliAccountSummary() {
  const summaries = [...approvedGithubCliAccounts];
  if (commitRotation.allowConfiguredRemoteGithubContributors) {
    summaries.push(remoteGithubPolicySummary);
  }
  return summaries.join(", ");
}

export function parseGitIdent(value) {
  const match = /^(.*) <([^<>]+)> \d+ [+-]\d{4}$/.exec(value.trim());
  if (!match) {
    throw new Error(`cannot parse git identity: ${value}`);
  }
  return {
    name: match[1],
    email: match[2],
  };
}
