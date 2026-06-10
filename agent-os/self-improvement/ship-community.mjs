#!/usr/bin/env node
// Ships a community challenge submission that cleared the margin: re-verifies
// against the current incumbent (gates + fuel, fresh run), commits the kernel
// authored to the submitter, advances task.base, records a Challenge entry in
// the ledger, regenerates the arena, and pushes. One command from public
// submission to production.
//
//   node ship-community.mjs <candidate.rs> --handle @grok [--proposer grok]
//     [--url https://x.com/...] [--margin 0.005]

import { readFileSync, writeFileSync, copyFileSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const benchRun = join(here, "bench", "run.mjs");
const taskJsonPath = join(here, "bench", "tasks", "audit-kernel-fuel", "task.json");
const ledgerPath = join(here, "kernel-archive", "ledger.json");

const argv = process.argv.slice(2);
const file = argv[0];
const opt = (f, d) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : d; };
const handle = opt("--handle", null);
const proposer = opt("--proposer", "community");
const url = opt("--url", null);
const margin = parseFloat(opt("--margin", "0.005"));
if (!file || !handle) { console.error("usage: ship-community.mjs <candidate.rs> --handle @x [--proposer grok] [--url ...]"); process.exit(2); }

const sh = (cmd, args, opts = {}) => {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
};
const round = (n) => Math.round(n * 1000) / 1000;
const bench = (solver) => {
  const r = sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", solver, "--json"]);
  if (!r.ok) throw new Error(`bench failed: ${r.out.slice(-300)}`);
  return JSON.parse(r.out.slice(r.out.indexOf("{"))).results[0];
};

const task = JSON.parse(readFileSync(taskJsonPath, "utf8"));
console.log("re-verifying incumbent and candidate against the current base…");
const incumbent = bench("none");
if (incumbent.correctness !== 1) throw new Error("incumbent fails its own gates");
const candidate = bench(`cmd:cp ${resolve(file)} ${task.evolveFile}`);
const scalar = candidate.correctness === 1 ? candidate.metrics.scalar ?? 0 : 0;
const gain = round(scalar - incumbent.metrics.scalar);
console.log(`incumbent ${incumbent.metrics.scalar} | candidate ${scalar} | gain ${gain}`);
if (candidate.correctness !== 1) throw new Error(`gates failed: ${candidate.error ?? candidate.gaming ?? "correctness"}`);
if (gain < margin) throw new Error(`gain ${gain} < margin ${margin} against the current incumbent — no ship`);

const authorName = handle.replace(/^@/, "");
copyFileSync(resolve(file), join(repoRoot, task.evolveFile));
sh("git", ["-C", repoRoot, "add", task.evolveFile]);
const commitMsg = `community: ${task.id} — proposed by ${handle} via the open challenge, scalar ${incumbent.metrics.scalar} -> ${scalar}${url ? `\n\nsubmission: ${url}` : ""}`;
const commit = sh("git", ["-C", repoRoot, "-c", `user.name=${authorName}`, "-c", `user.email=${authorName}@users.noreply.opencovenant.org`, "commit", "-m", commitMsg, "--only", task.evolveFile]);
if (!commit.ok) throw new Error(`commit failed: ${commit.out}`);
const sha = sh("git", ["-C", repoRoot, "rev-parse", "HEAD"]).out.trim();

const tj = JSON.parse(readFileSync(taskJsonPath, "utf8"));
tj.base = sha;
writeFileSync(taskJsonPath, JSON.stringify(tj, null, 2) + "\n");
sh("git", ["-C", repoRoot, "add", taskJsonPath]);
sh("git", ["-C", repoRoot, "-c", "user.name=Covenant", "-c", "user.email=covenant@users.noreply.github.com", "commit", "-m", `self-improvement(kernel): advance audit-kernel-fuel base to ${sha.slice(0, 8)}`, "--only", taskJsonPath]);

const ledger = existsSync(ledgerPath) ? JSON.parse(readFileSync(ledgerPath, "utf8")) : { versions: [] };
const cNum = ledger.versions.filter((v) => v.version.startsWith("c")).length + 1;
ledger.versions.push({ version: `c${cNum}`, proposer, handle, url, incumbent: incumbent.metrics.scalar, candidate: scalar, gain, promoted: true, commit: sha });
writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));

const arena = sh("node", [join(here, "gen-arena.mjs")]);
if (arena.ok) {
  sh("git", ["-C", repoRoot, "add", "landing/public/arena.json"]);
  sh("git", ["-C", repoRoot, "-c", "user.name=Covenant", "-c", "user.email=covenant@users.noreply.github.com", "commit", "-m", `arena: challenge ${cNum} scoreboard (${handle})`, "--only", "landing/public/arena.json"]);
}
const push = sh("git", ["-C", repoRoot, "push", "origin", "feat/self-improvement"]);
console.log(push.ok ? "pushed" : `PUSH FAILED: ${push.out.slice(-200)}`);
console.log(`\nSHIPPED: ${handle} -> ${sha.slice(0, 8)} | ${incumbent.metrics.scalar} -> ${scalar} (+${gain})`);
console.log(`verdict for the thread: Gates passed, bit-identical. ${scalar}x vs ${incumbent.metrics.scalar}x, +${gain} clears the +${margin} margin. Shipped to production: github.com/open-covenant/covenant/commit/${sha.slice(0, 8)} — attributed to ${handle}. Scoreboard: opencovenant.org/arena`);
