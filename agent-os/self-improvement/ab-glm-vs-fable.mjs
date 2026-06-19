#!/usr/bin/env node
// A/B: GLM-5.2 vs Claude Fable 5 on the same real, gated, scored coding task
// (audit-kernel-fuel). Read-only w.r.t. the arena — it scores candidates via
// the frozen bench but NEVER promotes, commits, or writes the ledger/board.
//
// For each model it runs N attempts (same prompt, same gates the arena uses)
// and reports: compile-success rate, gate-pass (bit-identical behavior) rate,
// and best scalar achieved. That measures, on covenant's own code, whether
// GLM's output quality matches Fable's — not a generic benchmark.
//
//   node ab-glm-vs-fable.mjs [--attempts 3]
// Needs GLM_API_KEY (or ZAI_API_KEY) in env or repo .env for the GLM arm.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const benchRun = join(here, "bench", "run.mjs");
const taskJsonPath = join(here, "bench", "tasks", "audit-kernel-fuel", "task.json");
const outDir = join(here, "kernel-archive", "ab");
mkdirSync(outDir, { recursive: true });

const argv = process.argv.slice(2);
const opt = (f, d) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : d; };
const attempts = parseInt(opt("--attempts", "3"), 10);
const fableModel = opt("--fable-model", "claude-fable-5");
const glmModel = opt("--glm-model", "glm-5.2");

const sh = (cmd, args, o = {}) => { const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...o }); return { ok: r.status === 0, out: `${r.stdout ?? ""}${r.stderr ?? ""}` }; };
const dotenv = (n) => process.env[n] || (() => { try { const l = readFileSync(join(repoRoot, ".env"), "utf8").split("\n").find((x) => x.startsWith(`${n}=`)); return l ? l.slice(n.length + 1).trim() : null; } catch { return null; } })();
const glmKey = dotenv("GLM_API_KEY") || dotenv("ZAI_API_KEY");

const START = "// EVOLVE-BLOCK-START";
const END = "// EVOLVE-BLOCK-END";
const task = JSON.parse(readFileSync(taskJsonPath, "utf8"));
const baseSrc = sh("git", ["-C", repoRoot, "show", `${task.base}:${task.evolveFile}`]).out;
const a = baseSrc.indexOf(START), b = baseSrc.indexOf(END);
const pre = baseSrc.slice(0, a), block = baseSrc.slice(a, b + END.length), post = baseSrc.slice(b + END.length);

const incumbent = JSON.parse(sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", "none", "--json"]).out.match(/\{[\s\S]*\}/)[0]).results[0];
const incScalar = incumbent.metrics.scalar;
console.log(`incumbent scalar ${incScalar} (base ${task.base.slice(0, 8)})\n`);

const prompt = `You are optimizing a Rust module for minimum wasmtime fuel (deterministic instruction count) on wasm32-wasip1, release profile. Current score: scalar ${incScalar}. Output ONLY the replacement code from \`${START}\` to \`${END}\` in one \`\`\`rust fence. Behavior must be bit-identical (held-out differential + corpus-digest gates). Safe Rust only; deps sha2/serde_json/serde/std::arch::wasm32; no new deps.

Current code:

${block}
`;

function extract(reply) {
  const i = reply.indexOf(START), j = reply.lastIndexOf(END);
  if (i < 0 || j <= i) return null;
  return reply.slice(i, j + END.length);
}

function callFable() {
  const scratch = join(outDir, "fable-scratch");
  mkdirSync(scratch, { recursive: true });
  const r = sh("claude", ["-p", prompt, "--model", fableModel, "--dangerously-skip-permissions"], { cwd: scratch });
  if (!r.ok) throw new Error(`claude failed: ${r.out.slice(-200)}`);
  return r.out;
}
function callGlm() {
  if (!glmKey) throw new Error("GLM_API_KEY / ZAI_API_KEY not set");
  const bodyFile = join(outDir, "glm-req.json");
  writeFileSync(bodyFile, JSON.stringify({ model: glmModel, messages: [{ role: "user", content: prompt }], max_tokens: 120000, thinking: { type: "enabled" } }));
  const r = sh("curl", ["-s", "-m", "1800", "-X", "POST", "https://api.z.ai/api/paas/v4/chat/completions", "-H", `Authorization: Bearer ${glmKey}`, "-H", "Content-Type: application/json", "--data", `@${bodyFile}`]);
  if (!r.ok) throw new Error(`glm request failed: ${r.out.slice(-200)}`);
  const d = JSON.parse(r.out);
  if (!d.choices) throw new Error(`glm error: ${JSON.stringify(d.error ?? d).slice(0, 200)}`);
  return d.choices[0].message.content;
}

async function runArm(name, call) {
  const res = { name, compiled: 0, gatePassed: 0, best: 0, attempts: [] };
  for (let i = 1; i <= attempts; i++) {
    let line = { attempt: i, scalar: 0, status: "" };
    try {
      const reply = call();
      const blk = extract(reply);
      if (!blk) { line.status = "no-block"; res.attempts.push(line); console.log(`  ${name} ${i}: no EVOLVE block`); continue; }
      const cand = join(outDir, `${name}-a${i}.rs`);
      writeFileSync(cand, pre + blk + post);
      const scored = JSON.parse(sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", `cmd:cp ${cand} ${task.evolveFile}`, "--json"]).out.match(/\{[\s\S]*\}/)[0]).results[0];
      const passed = scored.correctness === 1;
      // "compiled" ~ got far enough to be scored without a gaming/error abort on compile
      const compiled = !/does not compile|unclosed|error\[/.test(scored.error ?? "");
      if (compiled) res.compiled++;
      if (passed) { res.gatePassed++; res.best = Math.max(res.best, scored.metrics.scalar ?? 0); }
      line = { attempt: i, scalar: passed ? scored.metrics.scalar ?? 0 : 0, status: passed ? "gates-pass" : (scored.error ?? scored.gaming ?? "fail").toString().slice(0, 60) };
      console.log(`  ${name} ${i}: ${line.status}${passed ? ` scalar=${line.scalar}` : ""}`);
    } catch (e) { line.status = String(e.message).slice(0, 80); console.log(`  ${name} ${i}: ERR ${line.status}`); }
    res.attempts.push(line);
  }
  return res;
}

const arms = [];
console.log("GLM-5.2:");
arms.push(await runArm("glm", callGlm));
console.log("Fable 5:");
arms.push(await runArm("fable", callFable));

console.log(`\n=== A/B RESULT (n=${attempts} each, incumbent ${incScalar}) ===`);
for (const a of arms) {
  console.log(`${a.name.padEnd(6)} compile ${a.compiled}/${attempts} | gates-pass ${a.gatePassed}/${attempts} | best ${a.best || "-"}${a.best > incScalar ? ` (+${(a.best - incScalar).toFixed(3)} over incumbent)` : ""}`);
}
writeFileSync(join(outDir, "result.json"), JSON.stringify({ incScalar, attempts, arms }, null, 2));
