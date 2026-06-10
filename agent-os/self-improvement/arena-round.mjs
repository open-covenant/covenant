#!/usr/bin/env node
// One scheduled arena round, launchd-safe. Skips when the operator stop file
// exists, halts itself after 3 consecutive no-promotion rounds (touch
// .arena-dry and stop spending until an operator clears it), pushes the
// branch after each round so the live scoreboard at opencovenant.org/arena
// picks up the data, and logs everything to arena-runner.log.

import { readFileSync, writeFileSync, existsSync, appendFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const ledgerPath = join(here, "kernel-archive", "ledger.json");
const logPath = join(here, "arena-runner.log");
const stopFile = join(repoRoot, ".arena-stop");
const dryFile = join(repoRoot, ".arena-dry");
const DRY_LIMIT = 3;

const log = (msg) => {
  const line = `[${new Date().toISOString()}] ${msg}\n`;
  appendFileSync(logPath, line);
  process.stdout.write(line);
};

if (existsSync(stopFile)) { log("skip: .arena-stop present"); process.exit(0); }
if (existsSync(dryFile)) { log("skip: .arena-dry present (3 consecutive dry rounds; rm .arena-dry to resume)"); process.exit(0); }

// Consecutive dry rounds, counted across round numbers (a round may have
// several ledger entries; it is dry when none promoted).
const ledger = existsSync(ledgerPath) ? JSON.parse(readFileSync(ledgerPath, "utf8")) : { versions: [] };
const byRound = new Map();
for (const v of ledger.versions) {
  const n = parseInt(v.version.match(/^k(\d+)/)?.[1] ?? 0, 10);
  byRound.set(n, (byRound.get(n) ?? false) || !!v.promoted);
}
let dry = 0;
for (const n of [...byRound.keys()].sort((a, b) => b - a)) {
  if (byRound.get(n)) break;
  dry++;
}
if (dry >= DRY_LIMIT) {
  writeFileSync(dryFile, `dry since ${new Date().toISOString()} after ${dry} consecutive no-promotion rounds\n`);
  log(`HALT: ${dry} consecutive dry rounds — wrote .arena-dry, no further spend until cleared`);
  process.exit(0);
}

log(`round start (dry streak ${dry}/${DRY_LIMIT})`);
const r = spawnSync("node", [join(here, "optimize-code.mjs"), "--iters", "1", "--attempts", "3"], {
  cwd: repoRoot,
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
  timeout: 3 * 60 * 60 * 1000,
});
appendFileSync(logPath, `${r.stdout ?? ""}${r.stderr ?? ""}`);
log(`round finished (exit ${r.status})`);

const push = spawnSync("git", ["-C", repoRoot, "push", "origin", "feat/self-improvement"], { encoding: "utf8" });
log(push.status === 0 ? "pushed feat/self-improvement" : `PUSH FAILED: ${(push.stderr ?? "").slice(-200)}`);
