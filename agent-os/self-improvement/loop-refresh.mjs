#!/usr/bin/env node
// Lightweight observatory refresh: regenerate loop.json from the loop's
// ledger and push it if anything changed. Runs on a short timer (the arena
// scheduler only fires every 8h, too slow for "in flight"). Cheap — reads a
// file, no model calls. Tolerates push races with the arena runner via
// pull --rebase; a failed push just retries next tick.

import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const sh = (cmd, args) => spawnSync(cmd, args, { cwd: repoRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });

const gen = sh("node", [join(here, "gen-loop.mjs")]);
process.stdout.write(gen.stdout ?? "");
if (gen.status !== 0) { process.stderr.write(gen.stderr ?? ""); process.exit(0); }

const dirty = sh("git", ["status", "--porcelain", "landing/public/loop.json"]).stdout.trim();
if (!dirty) process.exit(0);

sh("git", ["add", "landing/public/loop.json"]);
sh("git", ["-c", "user.name=Covenant", "-c", "user.email=covenant@users.noreply.github.com", "commit", "-m", "arena: loop observatory refresh", "--only", "landing/public/loop.json"]);
sh("git", ["pull", "--rebase", "--autostash", "origin", "feat/self-improvement"]);
const push = sh("git", ["push", "origin", "feat/self-improvement"]);
console.log(push.status === 0 ? "pushed loop.json" : `push deferred: ${(push.stderr ?? "").slice(-120)}`);
