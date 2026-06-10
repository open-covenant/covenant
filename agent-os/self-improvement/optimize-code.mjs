#!/usr/bin/env node
// Track B: code evolution for the audit-kernel fuel task.
//
// Same skeleton as optimize.mjs (Track A), two substitutions: the evolving
// artifact is the EVOLVE block of covenant-audit-kernel/src/lib.rs instead of
// the scaffold, and the metric is wasmtime fuel (scalar = baseline/consumed,
// so margin 0.02 means a >=2% fuel cut). On promotion the winning kernel is
// committed and task.base advances (hill-climbing); the baseline fuel constant
// in grade/ stays frozen so scores remain monotone across versions.
// Executor != optimizer still holds: the proposer only writes the block; the
// frozen bench grades it in an isolated worktree.

import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const benchRun = join(here, "bench", "run.mjs");
const taskDir = join(here, "bench", "tasks", "audit-kernel-fuel");
const taskJsonPath = join(taskDir, "task.json");
const archiveDir = join(here, "kernel-archive");
const ledgerPath = join(archiveDir, "ledger.json");

const argv = process.argv.slice(2);
const opt = (f, d) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : d; };
const proposerModel = opt("--model", "claude-fable-5");
const margin = parseFloat(opt("--margin", "0.02"));
const iters = parseInt(opt("--iters", "1"), 10);
const round = (n) => Math.round(n * 1000) / 1000;

function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, status: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

function benchScore(solver) {
  const r = sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", solver, "--json"]);
  if (!r.ok) throw new Error(`bench run failed: ${r.out.slice(-400)}`);
  const rep = JSON.parse(r.out.slice(r.out.indexOf("{")));
  return rep.results[0];
}

const START = "// EVOLVE-BLOCK-START";
const END = "// EVOLVE-BLOCK-END";

function splitEvolve(src) {
  const a = src.indexOf(START);
  const b = src.indexOf(END);
  if (a < 0 || b <= a) throw new Error("EVOLVE markers missing");
  return { pre: src.slice(0, a), block: src.slice(a, b + END.length), post: src.slice(b + END.length) };
}

function extractBlock(reply) {
  const fence = reply.match(/```(?:rust)?\n([\s\S]*?)```/);
  const body = (fence ? fence[1] : reply).trim();
  if (!body.startsWith(START) || !body.endsWith(END)) throw new Error(`proposer output is not a complete EVOLVE block: ${body.slice(0, 120)}…`);
  return body;
}

const task = JSON.parse(readFileSync(taskJsonPath, "utf8"));
const ledger = existsSync(ledgerPath) ? JSON.parse(readFileSync(ledgerPath, "utf8")) : { versions: [] };
mkdirSync(archiveDir, { recursive: true });

for (let iter = 0; iter < iters; iter++) {
  const base = JSON.parse(readFileSync(taskJsonPath, "utf8")).base;
  const baseSrc = sh("git", ["-C", repoRoot, "show", `${base}:${task.evolveFile}`]);
  if (!baseSrc.ok) throw new Error(`cannot read kernel at base ${base}`);
  const { pre, block, post } = splitEvolve(baseSrc.out);

  const incumbent = benchScore("none");
  if (incumbent.correctness !== 1) throw new Error(`incumbent fails its own gates: ${JSON.stringify(incumbent)}`);
  console.log(`[${iter + 1}/${iters}] incumbent scalar=${incumbent.metrics.scalar} (base ${base.slice(0, 8)})`);

  const history = ledger.versions
    .slice(-6)
    .map((v) => `- ${v.version}: scalar ${v.candidate} vs ${v.incumbent} -> ${v.promoted ? "PROMOTED" : `rejected (${v.reason})`}`)
    .join("\n") || "(no prior attempts)";

  const prompt = `You are optimizing a Rust module for minimum wasmtime fuel (deterministic instruction count) on wasm32-wasip1, release profile. Current score: scalar ${incumbent.metrics.scalar} (baseline_fuel/your_fuel — higher is better; you must beat it by >=${margin}).

Rules:
- Output ONLY the replacement code, starting with \`${START}\` and ending with \`${END}\`, inside one \`\`\`rust fence. No commentary.
- \`imp::verify_chain\` and \`imp::fold_chain\` keep their exact signatures; they are called from frozen wrappers outside the block.
- Observable behavior must be bit-identical: same ChainReport / ChainEntry values, same failure kinds in the same order. Gates: unit tests pinning exact lowercase sha256 hex and the prev\\nhash composition, a held-out differential suite against a frozen reference, and a frozen 50k-event corpus report digest.
- Deps: sha2, serde_json, serde only. No new dependencies. #![forbid(unsafe_code)] is outside the block and enforced.
- Fuel counts every instruction including the allocator: allocation elimination, incremental hashing, stack buffers, and cheaper JSON field extraction all pay.

Recent attempts:
${history}

Current code:

${block}
`;

  const prop = sh("claude", ["-p", prompt, "--model", proposerModel, "--dangerously-skip-permissions"]);
  const stamp = `k${ledger.versions.length + 1}`;
  let block;
  try {
    if (!prop.ok) throw new Error(`proposer failed: ${prop.out.slice(-400)}`);
    block = extractBlock(prop.out);
  } catch (e) {
    // A malformed proposal is a rejection, not a crash — log it and move on.
    ledger.versions.push({ version: stamp, incumbent: incumbent.metrics.scalar, candidate: 0, gain: 0, promoted: false, reason: `malformed proposal: ${String(e.message).slice(0, 200)}` });
    writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));
    console.log(`REJECTED ${stamp}: malformed proposal`);
    continue;
  }
  const candidateSrc = pre + block + post;
  const candidateFile = join(archiveDir, `${stamp}-candidate.rs`);
  writeFileSync(candidateFile, candidateSrc);

  const candidate = benchScore(`cmd:cp ${candidateFile} ${task.evolveFile}`);
  const gain = round((candidate.metrics.scalar ?? 0) - incumbent.metrics.scalar);
  const promote = candidate.correctness === 1 && gain >= margin;
  console.log(`[${iter + 1}/${iters}] candidate scalar=${candidate.metrics.scalar ?? 0} gain=${gain}${candidate.gaming?.length ? ` GAMING ${candidate.gaming.join("; ")}` : ""}${candidate.error ? ` ERROR ${candidate.error}` : ""}`);

  if (promote) {
    writeFileSync(join(repoRoot, task.evolveFile), candidateSrc);
    const add = sh("git", ["-C", repoRoot, "add", task.evolveFile]);
    if (!add.ok) throw new Error(`git add failed: ${add.out}`);
    const commit = sh("git", [
      "-C", repoRoot,
      "-c", "user.name=Covenant",
      "-c", "user.email=covenant@users.noreply.github.com",
      "commit", "-m", `self-improvement(kernel): ${stamp} fuel scalar ${incumbent.metrics.scalar} -> ${candidate.metrics.scalar}`,
      "--only", task.evolveFile,
    ]);
    if (!commit.ok) throw new Error(`git commit failed: ${commit.out}`);
    const sha = sh("git", ["-C", repoRoot, "rev-parse", "HEAD"]).out.trim();
    const tj = JSON.parse(readFileSync(taskJsonPath, "utf8"));
    tj.base = sha;
    writeFileSync(taskJsonPath, JSON.stringify(tj, null, 2) + "\n");
    sh("git", ["-C", repoRoot, "add", taskJsonPath]);
    sh("git", [
      "-C", repoRoot,
      "-c", "user.name=Covenant",
      "-c", "user.email=covenant@users.noreply.github.com",
      "commit", "-m", `self-improvement(kernel): advance audit-kernel-fuel base to ${sha.slice(0, 8)}`,
      "--only", taskJsonPath,
    ]);
    ledger.versions.push({ version: stamp, incumbent: incumbent.metrics.scalar, candidate: candidate.metrics.scalar, gain, promoted: true, commit: sha });
    console.log(`PROMOTED ${stamp}: scalar ${incumbent.metrics.scalar} -> ${candidate.metrics.scalar} (commit ${sha.slice(0, 8)}, base advanced)`);
  } else {
    const reason = candidate.gaming?.length ? `gaming: ${candidate.gaming.join("; ")}`
      : candidate.error ? `error: ${candidate.error}`
      : candidate.correctness !== 1 ? "correctness gate failed"
      : `gain ${gain} < margin ${margin}`;
    ledger.versions.push({ version: stamp, incumbent: incumbent.metrics.scalar, candidate: candidate.metrics.scalar ?? 0, gain, promoted: false, reason });
    console.log(`REJECTED ${stamp}: ${reason}`);
  }
  writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));
}
