#!/usr/bin/env node
// Held-out wasm-execution gate. Candidates may add wasm-only code paths
// (cfg(target_arch = "wasm32"), #[target_feature]) that native test runs
// never execute — k5 introduced a simd128 lane and proved the gap. This
// stage compiles the kernel's unit + differential test suites to
// wasm32-wasip1 and executes them under the pinned wasmtime runner, so the
// exact code the fuel metric measures is also the code the tests verify.

import { spawnSync } from "node:child_process";
import { writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const die = (msg) => { console.error(msg); process.exit(1); };
const benchRoot = process.env.COVENANT_BENCH_ROOT;
if (!benchRoot) die("COVENANT_BENCH_ROOT not set — run this stage via bench/run.mjs");
const runner = join(benchRoot, "runners", "fuel-runner", "target", "release", "fuel-runner");
// Self-heal: rebuild the runner if missing (e.g. target/ was wiped). The
// fuel-stage does this too; this gate runs earlier, so it must as well.
if (!existsSync(runner)) {
  const b = spawnSync("cargo", ["build", "--release"], { cwd: join(benchRoot, "runners", "fuel-runner"), encoding: "utf8" });
  if (b.status !== 0) die(`fuel-runner build failed: ${(b.stderr ?? "").slice(-400)}`);
}

const build = spawnSync(
  "cargo",
  ["test", "-p", "covenant-audit-kernel", "--target", "wasm32-wasip1", "--tests", "--no-run", "--message-format=json"],
  { cwd: "agent-os", encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
);
if (build.status !== 0) die(`wasm test build failed: ${build.stderr.slice(-600)}`);

const wasms = build.stdout
  .split("\n")
  .filter(Boolean)
  .map((l) => { try { return JSON.parse(l); } catch { return {}; } })
  .filter((m) => m.reason === "compiler-artifact" && m.executable && m.executable.endsWith(".wasm"))
  .map((m) => m.executable);
if (wasms.length < 2) die(`expected >=2 wasm test binaries, got ${wasms.length}`);

const empty = "/tmp/covenant-wasm-tests-empty.bin";
if (!existsSync(empty)) writeFileSync(empty, "");

for (const wasm of wasms) {
  const run = spawnSync(runner, [wasm, empty], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  const out = `${run.stdout}${run.stderr}`;
  const ok = run.status === 0 && /test result: ok\./.test(out) && !/FAILED|panicked/.test(out);
  console.log(`${wasm.split("/").pop()}: ${ok ? "ok" : "FAILED"}`);
  if (!ok) die(out.slice(-800));
}
