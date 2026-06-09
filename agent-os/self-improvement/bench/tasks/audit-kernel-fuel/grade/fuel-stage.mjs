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

console.log(`FUEL ${fuel}`);
console.log(`SCALAR ${scalar}`);
