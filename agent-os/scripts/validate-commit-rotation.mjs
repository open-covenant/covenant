#!/usr/bin/env node
import { commitRotationPath, loadCommitRotation } from "./commit-rotation.mjs";

function identityKey(identity) {
  return `${identity.name}\0${identity.email}`;
}

function fail(errors) {
  console.error("validate-commit-rotation: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const errors = [];
let rotation;

try {
  rotation = loadCommitRotation();
} catch (error) {
  fail([`${commitRotationPath}: ${error.message}`]);
}

const identityKeys = new Set();
for (const identity of rotation.approvedGitIdentities) {
  const key = identityKey(identity);
  if (identityKeys.has(key)) {
    errors.push(`duplicate approved Git identity: ${identity.name} <${identity.email}>`);
  }
  identityKeys.add(key);
}

if (!identityKeys.has(identityKey(rotation.currentGitIdentity))) {
  errors.push(
    `currentGitIdentity must appear in approvedGitIdentities: ${rotation.currentGitIdentity.name} <${rotation.currentGitIdentity.email}>`,
  );
}

const githubAccounts = new Set();
for (const account of rotation.approvedGithubAccounts) {
  if (githubAccounts.has(account)) {
    errors.push(`duplicate approved GitHub account: ${account}`);
  }
  githubAccounts.add(account);
}

if (identityKeys.size === 0 && !rotation.allowConfiguredRemoteGitIdentities) {
  errors.push("at least one approved Git identity is required when remote Git identities are disabled");
}

if (githubAccounts.size === 0 && !rotation.allowConfiguredRemoteGithubContributors) {
  errors.push(
    "at least one approved GitHub account is required when remote GitHub contributors are disabled",
  );
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-commit-rotation: ok");
