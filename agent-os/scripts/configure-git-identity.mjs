#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { activeProjectGitIdentity } from "./git-identity-policy.mjs";

const git = (args) =>
  spawnSync("git", args, {
    encoding: "utf8",
  });

const run = (args) => {
  const result = git(args);
  if (result.status !== 0) {
    const detail = (result.stderr || result.stdout).trim();
    throw new Error(`git ${args.join(" ")} failed: ${detail}`);
  }
};

const identity = activeProjectGitIdentity();

run(["config", "user.name", identity.name]);
run(["config", "user.email", identity.email]);

console.log(`configure-git-identity: ok (${identity.name} <${identity.email}>)`);
