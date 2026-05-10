import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
export const commitRotationPath = join(scriptDir, "..", "autonomy", "commit-rotation.json");

const schema = "covenant.commit-rotation.v1";
const defaultCommitRotation = {
  schema,
  remote: "origin",
  currentGitIdentity: {
    name: "Open Covenant Automation",
    email: "opencovenant@users.noreply.github.com",
  },
  approvedGitIdentities: [
    {
      name: "Open Covenant Automation",
      email: "opencovenant@users.noreply.github.com",
    },
  ],
  approvedGithubAccounts: ["OpenCovenant"],
  allowConfiguredRemoteGitIdentities: true,
  allowConfiguredRemoteGithubContributors: true,
};

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requiredString(value, path, errors) {
  if (typeof value !== "string" || value.trim() === "") {
    errors.push(`${path} must be a non-empty string`);
    return "";
  }
  return value.trim();
}

function requiredBoolean(value, path, errors) {
  if (typeof value !== "boolean") {
    errors.push(`${path} must be a boolean`);
    return false;
  }
  return value;
}

function parseIdentity(value, path, errors) {
  if (!isObject(value)) {
    errors.push(`${path} must be an object`);
    return { name: "", email: "" };
  }

  return {
    name: requiredString(value.name, `${path}.name`, errors),
    email: requiredString(value.email, `${path}.email`, errors),
  };
}

function parseIdentityList(value, path, errors) {
  if (!Array.isArray(value)) {
    errors.push(`${path} must be an array`);
    return [];
  }

  return value.map((identity, index) => parseIdentity(identity, `${path}[${index}]`, errors));
}

function parseAccountList(value, path, errors) {
  if (!Array.isArray(value)) {
    errors.push(`${path} must be an array`);
    return [];
  }

  return value.map((account, index) => requiredString(account, `${path}[${index}]`, errors));
}

function cloneRotation(rotation) {
  return {
    ...rotation,
    currentGitIdentity: { ...rotation.currentGitIdentity },
    approvedGitIdentities: rotation.approvedGitIdentities.map((identity) => ({ ...identity })),
    approvedGithubAccounts: [...rotation.approvedGithubAccounts],
  };
}

function parseCommitRotation(data) {
  const errors = [];

  if (!isObject(data)) {
    throw new Error("commit rotation file must contain an object");
  }

  if (data.schema !== schema) {
    errors.push(`schema must be ${schema}`);
  }

  const rotation = {
    schema,
    remote: requiredString(data.remote, "remote", errors),
    currentGitIdentity: parseIdentity(data.currentGitIdentity, "currentGitIdentity", errors),
    approvedGitIdentities: parseIdentityList(
      data.approvedGitIdentities,
      "approvedGitIdentities",
      errors,
    ),
    approvedGithubAccounts: parseAccountList(
      data.approvedGithubAccounts,
      "approvedGithubAccounts",
      errors,
    ),
    allowConfiguredRemoteGitIdentities: requiredBoolean(
      data.allowConfiguredRemoteGitIdentities,
      "allowConfiguredRemoteGitIdentities",
      errors,
    ),
    allowConfiguredRemoteGithubContributors: requiredBoolean(
      data.allowConfiguredRemoteGithubContributors,
      "allowConfiguredRemoteGithubContributors",
      errors,
    ),
  };

  if (errors.length > 0) {
    throw new Error(errors.join("; "));
  }

  return rotation;
}

export function loadCommitRotation(path = commitRotationPath) {
  let data;

  try {
    data = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") {
      return cloneRotation(defaultCommitRotation);
    }
    throw new Error(`cannot read commit rotation file: ${error.message}`);
  }

  return parseCommitRotation(data);
}
