#!/usr/bin/env node
// Held-out fuel metric stage. Runs in the candidate worktree root:
// builds kernel_bench.wasm, regenerates the frozen corpus into the engine
// cache if needed, meters fuel with the pinned wasmtime runner, and gates on
// the report digest (any behavioral drift on the corpus fails the stage).
// Prints `SCALAR baseline/consumed` for the bench's scalar metric.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

const BASELINE_FUEL = 5867618602n;
const CORPUS_SHA = "eec86e93bd6cac37efe4c1efccc836072d421f6ff01da08d84a7665660347d8e";
const DIGEST = "cbc91600fdcadb7239637d88ab640c97497e41e853c7f4ba7d191ae6e919d2e8";
// Hidden corpus: a second 50k-event set on a different seed/distribution.
// Behavior digest only (fuel stays measured on the public corpus so scores
// stay comparable). A candidate that is faster only because it overfits the
// public corpus's specific byte layout — but diverges on inputs it never
// saw — fails here. Anti-overfit safety net.
const HIDDEN_SEED = "7";
const HIDDEN_CORPUS_SHA = "f535efa8925bc520459650472761d65528256e003748815abee0e787a0a6269d";
const HIDDEN_DIGEST = "04e7214e1c7e69890ec848f2ba613526c0e6bbd517c964bf95447a304fca9683";

const sh = (cmd, args, opts = {}) => {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
};
const die = (msg) => { console.error(msg); process.exit(1); };

const benchRoot = process.env.COVENANT_BENCH_ROOT;
if (!benchRoot) die("COVENANT_BENCH_ROOT not set — run this stage via bench/run.mjs");
const runnersDir = join(benchRoot, "runners");

const runner = join(runnersDir, "fuel-runner", "target", "release", "fuel-runner");
if (!existsSync(runner)) {
  const b = sh("cargo", ["build", "--release"], { cwd: join(runnersDir, "fuel-runner") });
  if (!b.ok) die(`fuel-runner build failed: ${b.out.slice(-400)}`);
}

const cacheDir = join(runnersDir, ".cache");
mkdirSync(cacheDir, { recursive: true });
const corpus = join(cacheDir, "audit-fuel-corpus-seed1.bin");
const sha = (f) => createHash("sha256").update(readFileSync(f)).digest("hex");
if (!existsSync(corpus) || sha(corpus) !== CORPUS_SHA) {
  const g = sh("node", [join(runnersDir, "gen-audit-corpus.mjs"), corpus, "--seed", "1"]);
  if (!g.ok) die(`corpus generation failed: ${g.out.slice(-400)}`);
  if (sha(corpus) !== CORPUS_SHA) die("regenerated corpus sha mismatch — generator drifted");
}

const wasmBuild = sh(
  "cargo",
  ["build", "--release", "--target", "wasm32-wasip1", "-p", "covenant-audit-kernel", "--features", "bench-bin"],
  { cwd: "agent-os" },
);
if (!wasmBuild.ok) die(`wasm build failed: ${wasmBuild.out.slice(-600)}`);

const wasm = join("agent-os", "target", "wasm32-wasip1", "release", "kernel_bench.wasm");
const run = sh(runner, [wasm, corpus, "--baseline", String(BASELINE_FUEL)]);
if (!run.ok) die(`fuel run failed: ${run.out.slice(-400)}`);

const digest = run.out.match(/^DIGEST ([0-9a-f]{64})/m)?.[1];
const scalar = run.out.match(/^SCALAR ([\d.]+)/m)?.[1];
const fuel = run.out.match(/^FUEL (\d+)/m)?.[1];
if (digest !== DIGEST) die(`behavioral drift: digest ${digest} != frozen ${DIGEST}`);
if (!scalar || !fuel) die(`runner output missing FUEL/SCALAR: ${run.out.slice(-200)}`);

// Anti-overfit: behavior must hold on the hidden corpus too.
const hidden = join(cacheDir, "audit-fuel-corpus-seed7.bin");
if (!existsSync(hidden) || sha(hidden) !== HIDDEN_CORPUS_SHA) {
  const g = sh("node", [join(runnersDir, "gen-audit-corpus.mjs"), hidden, "--seed", HIDDEN_SEED]);
  if (!g.ok) die(`hidden corpus generation failed: ${g.out.slice(-400)}`);
  if (sha(hidden) !== HIDDEN_CORPUS_SHA) die("hidden corpus sha mismatch — generator drifted");
}
const hiddenRun = sh(runner, [wasm, hidden]);
if (!hiddenRun.ok) die(`hidden corpus run failed: ${hiddenRun.out.slice(-400)}`);
const hiddenDigest = hiddenRun.out.match(/^DIGEST ([0-9a-f]{64})/m)?.[1];
if (hiddenDigest !== HIDDEN_DIGEST) die(`behavioral drift on the hidden corpus: ${hiddenDigest} != frozen ${HIDDEN_DIGEST} (your change diverges on inputs outside the public 50k set)`);

console.log(`FUEL ${fuel}`);
console.log(`SCALAR ${scalar}`);
