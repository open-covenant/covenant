#!/usr/bin/env node
// Track A: recursive scaffold optimizer.
//
// Reads the current coding scaffold + the benchmark's failures, asks a separate
// `claude -p` (the PROPOSER, distinct from the executor that runs inside the bench)
// for one minimal scaffold edit, re-scores the candidate on the frozen benchmark,
// and promotes it only if it beats the incumbent by a margin without regressing a
// held-out correctness gate. Every version is archived to a ledger; nothing is
// deleted. This optimizer and the bench are immutable core; only the scaffold and
// the archive are writable. Executor != optimizer (invariant 4).

import { readFileSync, writeFileSync, mkdirSync, existsSync, copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const benchRun = join(here, "bench", "run.mjs");
const scaffoldPath = join(here, "scaffold", "coder.md");
const archiveDir = join(here, "scaffold", "archive");
const ledgerPath = join(archiveDir, "ledger.json");

const argv = process.argv.slice(2);
const opt = (f, d) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : d; };
const proposerModel = opt("--model", "claude-opus-4-8");
const solverModel = opt("--scaffold-model", "claude-opus-4-8");
const margin = parseFloat(opt("--margin", "0.02"));
const evalRuns = parseInt(opt("--eval-runs", "1"), 10);
const taskFilter = opt("--task", null);
const round = (n) => Math.round(n * 1000) / 1000;

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, status: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

// Worst meanScore over evalRuns — a cheap lower-confidence bound against noisy evals.
function scoreScaffold(file) {
  let report = null;
  for (let i = 0; i < evalRuns; i++) {
    const args = [benchRun, "--solver", "scaffold", "--scaffold", file, "--scaffold-model", solverModel, "--json"];
    if (taskFilter) args.push("--task", taskFilter);
    const r = sh("node", args);
    if (!r.ok) throw new Error(`bench run failed: ${r.out.slice(-400)}`);
    const rep = JSON.parse(r.out.slice(r.out.indexOf("{")));
    if (!report || rep.meanScore < report.meanScore) report = rep;
  }
  return report;
}

function archive(label, file) {
  mkdirSync(archiveDir, { recursive: true });
  copyFileSync(file, join(archiveDir, `${label}.md`));
}

const ledger = existsSync(ledgerPath) ? JSON.parse(readFileSync(ledgerPath, "utf8")) : { versions: [] };

const incumbent = scoreScaffold(scaffoldPath);
console.log(`incumbent meanScore=${incumbent.meanScore}`);

const current = readFileSync(scaffoldPath, "utf8");
const failing = incumbent.results
  .filter((r) => r.score < 1)
  .map((r) => `- ${r.id}: score ${r.score} ${JSON.stringify(r.metrics)}${r.error ? " err:" + r.error : ""}`)
  .join("\n") || "(every task already scores 1 on this benchmark)";

const proposerPrompt = `You are optimizing a CODING SCAFFOLD: the system prompt a coding agent follows to solve tasks in a repository. The scaffold is scored by a frozen, held-out benchmark. Propose ONE minimal, high-leverage edit so the agent solves MORE tasks correctly.

Current scaffold:
<scaffold>
${current}
</scaffold>

Latest benchmark (meanScore ${incumbent.meanScore}); tasks scoring below 1:
${failing}

Output ONLY the full revised scaffold as markdown. No commentary, no code fences. Keep it concise and general; never overfit to a specific task or name task ids.`;

const prop = sh("claude", ["-p", proposerPrompt, "--model", proposerModel, "--dangerously-skip-permissions"]);
if (!prop.ok) throw new Error(`proposer failed: ${prop.out.slice(-400)}`);
const candidateText = prop.out.trim().replace(/^```(?:markdown)?\n?/, "").replace(/\n?```$/, "");
mkdirSync(archiveDir, { recursive: true });
const candidateFile = join(archiveDir, "candidate.md");
writeFileSync(candidateFile, candidateText);

const candidate = scoreScaffold(candidateFile);
console.log(`candidate meanScore=${candidate.meanScore}`);

const regressed = incumbent.results.some((ir) => {
  const cr = candidate.results.find((x) => x.id === ir.id);
  return ir.correctness === 1 && cr && cr.correctness === 0;
});
const gain = round(candidate.meanScore - incumbent.meanScore);
const promote = gain >= margin && !regressed;
const stamp = `v${ledger.versions.length + 1}`;

if (promote) {
  archive(`${stamp}-prev`, scaffoldPath);
  copyFileSync(candidateFile, scaffoldPath);
  ledger.versions.push({ version: stamp, incumbent: incumbent.meanScore, candidate: candidate.meanScore, gain, promoted: true });
  console.log(`PROMOTED ${stamp}: ${incumbent.meanScore} -> ${candidate.meanScore} (+${gain})`);
} else {
  archive(`${stamp}-rejected`, candidateFile);
  const reason = regressed ? "held-out regression" : `gain ${gain} < margin ${margin}`;
  ledger.versions.push({ version: stamp, incumbent: incumbent.meanScore, candidate: candidate.meanScore, gain, promoted: false, reason });
  console.log(`REJECTED ${stamp}: candidate ${candidate.meanScore} vs incumbent ${incumbent.meanScore} (${reason})`);
}
writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));
