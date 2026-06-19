#!/usr/bin/env node
// A/B: GLM-5.2 vs Claude Fable 5 on covenant's own gated coding task, using
// the FUNCTION LANE — the fair format for any proposer (whole-block forces a
// ~39k-token reproduction that truncates reasoning models; the function lane
// is what made Codex competitive). Each model picks ONE function in the audit
// kernel, returns `FN: <name>` + the replacement; the frozen bench scores the
// whole kernel with it spliced in. Identical CLI agent path (claude -p) for
// both — only the backend model differs.
//
// Read-only w.r.t. the arena: scores candidates, never promotes/commits/writes
// the ledger or board.
//
//   node ab-glm-vs-fable.mjs [--attempts 3]
// Needs GLM_API_KEY (z.ai Coding Plan) in env or repo .env for the GLM arm.

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
const block = baseSrc.slice(a, b + END.length);

const incumbent = JSON.parse(sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", "none", "--json"]).out.match(/\{[\s\S]*\}/)[0]).results[0];
const incScalar = incumbent.metrics.scalar;
console.log(`incumbent scalar ${incScalar} (base ${task.base.slice(0, 8)})\n`);

const prompt = `You are optimizing a single Rust function for minimum wasmtime fuel (deterministic instruction count) on wasm32-wasip1. The kernel below is already heavily optimized; do NOT rewrite the whole thing. Pick ONE function whose fuel you can reduce (hot paths: byte scanning, line splitting, sha256, hex encoding, JSON field scanning) and return ONLY an improved replacement.

Output format, exactly:
- First line: \`FN: <function_name>\`
- Then the complete replacement function in one \`\`\`rust fence, with its #[cfg]/#[target_feature] attributes. Same signature, behavior bit-identical.

Safe Rust only; deps sha2/serde_json/serde/std::arch::wasm32; no new deps. Beat incumbent scalar ${incScalar}.

Kernel:

${block}
`;

// --- function splice (mirrors optimize-code.mjs) ---
function itemEnd(src, braceStart) { let d = 0; for (let i = braceStart; i < src.length; i++) { const c = src[i]; if (c === '"') { i++; while (i < src.length && src[i] !== '"') i += src[i] === "\\" ? 2 : 1; } else if (c === "/" && src[i + 1] === "/") { while (i < src.length && src[i] !== "\n") i++; } else if (c === "{") d++; else if (c === "}") { d--; if (d === 0) return i + 1; } } throw new Error("unbalanced braces"); }
function itemStart(src, fnIdx) { let ls = src.lastIndexOf("\n", fnIdx) + 1; while (true) { const ps = src.lastIndexOf("\n", ls - 2) + 1; const pv = src.slice(ps, ls).trim(); if (pv.startsWith("#[") || pv.startsWith("///") || pv.startsWith("//")) ls = ps; else break; } return ls; }
function spliceFn(source, name, item) { const h = [...source.matchAll(new RegExp(`\\bfn\\s+${name}\\s*[(<]`, "g"))]; if (!h.length) throw new Error(`fn ${name} not in kernel`); const fi = h[0].index; const s = itemStart(source, fi), e = itemEnd(source, source.indexOf("{", fi)); const ind = "    "; const it = item.split("\n").map((l) => (l.trim() ? ind + l : l)).join("\n").replace(/^ +/, ind); return source.slice(0, s) + it + "\n" + source.slice(e).replace(/^\n/, ""); }
function extractFn(reply) { const fn = reply.match(/FN:\s*([A-Za-z0-9_]+)/); const fc = reply.match(/```(?:rust)?\n([\s\S]*?)```/); if (!fn) throw new Error("no FN: line"); const body = (fc ? fc[1] : reply).trim(); if (!body.includes(`fn ${fn[1]}`)) throw new Error(`no fn ${fn[1]} in body`); return { name: fn[1], body }; }

function callClaude(model, extraEnv) {
  const scratch = join(outDir, `${model}-scratch`);
  mkdirSync(scratch, { recursive: true });
  const r = sh("claude", ["-p", prompt, "--model", model, "--dangerously-skip-permissions"], { cwd: scratch, env: { ...process.env, ...extraEnv } });
  if (!r.ok) throw new Error(`claude(${model}) failed: ${r.out.slice(-200)}`);
  return r.out;
}
const callGlm = () => { if (!glmKey) throw new Error("GLM_API_KEY not set"); return callClaude(glmModel, { ANTHROPIC_BASE_URL: "https://api.z.ai/api/anthropic", ANTHROPIC_AUTH_TOKEN: glmKey, API_TIMEOUT_MS: "3000000" }); };
const callFable = () => callClaude(fableModel, {});

async function runArm(name, call) {
  const res = { name, valid: 0, gatePassed: 0, best: 0, picks: [], attempts: [] };
  for (let i = 1; i <= attempts; i++) {
    let line = { attempt: i, scalar: 0, status: "" };
    try {
      const reply = call();
      let parsed;
      try { parsed = extractFn(reply); } catch (e) { line.status = `parse: ${e.message}`; console.log(`  ${name} ${i}: ${line.status}`); res.attempts.push(line); continue; }
      res.valid++; res.picks.push(parsed.name);
      const cand = join(outDir, `${name}-a${i}.rs`);
      writeFileSync(cand, spliceFn(baseSrc, parsed.name, parsed.body));
      const scored = JSON.parse(sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", `cmd:cp ${cand} ${task.evolveFile}`, "--json"]).out.match(/\{[\s\S]*\}/)[0]).results[0];
      const passed = scored.correctness === 1;
      if (passed) { res.gatePassed++; res.best = Math.max(res.best, scored.metrics.scalar ?? 0); }
      line = { attempt: i, scalar: passed ? scored.metrics.scalar ?? 0 : 0, status: passed ? `gates-pass (${parsed.name})` : `gate-fail (${parsed.name}): ${(scored.error ?? scored.gaming ?? "fail").toString().slice(0, 50)}` };
      console.log(`  ${name} ${i}: ${line.status}${passed ? ` scalar=${line.scalar}` : ""}`);
    } catch (e) { line.status = String(e.message).slice(0, 90); console.log(`  ${name} ${i}: ERR ${line.status}`); }
    res.attempts.push(line);
  }
  return res;
}

const arms = [];
console.log("GLM-5.2 (z.ai Coding Plan, via claude -p):");
arms.push(await runArm("glm", callGlm));
console.log("Fable 5:");
arms.push(await runArm("fable", callFable));

console.log(`\n=== A/B RESULT — function lane, n=${attempts} each, incumbent ${incScalar} ===`);
for (const a of arms) {
  console.log(`${a.name.padEnd(6)} valid ${a.valid}/${attempts} | gates-pass ${a.gatePassed}/${attempts} | best ${a.best || "-"}${a.best > incScalar ? ` (+${(a.best - incScalar).toFixed(3)})` : ""} | picked: ${a.picks.join(", ") || "-"}`);
}
writeFileSync(join(outDir, "result.json"), JSON.stringify({ incScalar, attempts, lane: "function", arms }, null, 2));
console.log("\n(arena board untouched — A/B scores only, no promote/commit)");
