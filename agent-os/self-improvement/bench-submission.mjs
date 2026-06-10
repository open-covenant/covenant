#!/usr/bin/env node
// Scores a community challenge submission: splices a replacement function
// into the incumbent kernel and runs it through the full gate stack in an
// isolated worktree. Score-only — promotion stays with the operator.
//
//   node bench-submission.mjs <submission.rs> [--fn find_newline]
//     [--occurrence 1] [--handle @someone]
//
// The submission file must contain the complete replacement item including
// any attributes (e.g. #[cfg(target_arch = "wasm32")]). Occurrence 1 of a
// name is the first definition in the file (for find_newline: the wasm path
// the fuel meter actually measures).

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const benchRun = join(here, "bench", "run.mjs");
const taskJsonPath = join(here, "bench", "tasks", "audit-kernel-fuel", "task.json");
const archiveDir = join(here, "kernel-archive");

const argv = process.argv.slice(2);
const file = argv[0];
const opt = (f, d) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : d; };
const fnName = opt("--fn", "find_newline");
const occurrence = parseInt(opt("--occurrence", "1"), 10);
const handle = opt("--handle", "community");
if (!file) { console.error("usage: bench-submission.mjs <submission.rs> [--fn name] [--occurrence n] [--handle @x]"); process.exit(2); }

const sh = (cmd, args, opts = {}) => {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
};

// Find the end of the item starting at the fn's opening brace: brace count
// that skips string/char/byte literals and line comments.
function itemEnd(src, braceStart) {
  let depth = 0;
  for (let i = braceStart; i < src.length; i++) {
    const c = src[i];
    if (c === '"') {
      i++;
      while (i < src.length && src[i] !== '"') i += src[i] === "\\" ? 2 : 1;
    } else if (c === "'" && /['\\]|^[^']{1,2}'/.test(src.slice(i + 1, i + 4))) {
      i++;
      while (i < src.length && src[i] !== "'") i += src[i] === "\\" ? 2 : 1;
    } else if (c === "/" && src[i + 1] === "/") {
      while (i < src.length && src[i] !== "\n") i++;
    } else if (c === "{") depth++;
    else if (c === "}") { depth--; if (depth === 0) return i + 1; }
  }
  throw new Error("unbalanced braces");
}

// Walk back from the fn line over contiguous attribute/doc lines.
function itemStart(src, fnIdx) {
  let lineStart = src.lastIndexOf("\n", fnIdx) + 1;
  while (true) {
    const prevStart = src.lastIndexOf("\n", lineStart - 2) + 1;
    const prev = src.slice(prevStart, lineStart).trim();
    if (prev.startsWith("#[") || prev.startsWith("///") || prev.startsWith("//")) {
      lineStart = prevStart;
    } else break;
  }
  return lineStart;
}

const task = JSON.parse(readFileSync(taskJsonPath, "utf8"));
const baseSrc = sh("git", ["-C", repoRoot, "show", `${task.base}:${task.evolveFile}`]);
if (!baseSrc.ok) { console.error(`cannot read kernel at base ${task.base}`); process.exit(1); }
const src = baseSrc.out;

const needle = new RegExp(`\\bfn\\s+${fnName}\\s*[(<]`, "g");
const hits = [...src.matchAll(needle)];
if (hits.length < occurrence) { console.error(`found ${hits.length} definition(s) of ${fnName}, wanted occurrence ${occurrence}`); process.exit(1); }
const fnIdx = hits[occurrence - 1].index;
const start = itemStart(src, fnIdx);
const end = itemEnd(src, src.indexOf("{", fnIdx));

const submission = readFileSync(file, "utf8").trim();
if (!submission.includes(`fn ${fnName}`)) { console.error(`submission does not define fn ${fnName}`); process.exit(1); }
const indent = "    ";
const indented = submission.split("\n").map((l) => (l.trim() ? indent + l : l)).join("\n").replace(/^ +/, indent);
const candidateSrc = src.slice(0, start) + indented + "\n" + src.slice(end).replace(/^\n/, "");

mkdirSync(archiveDir, { recursive: true });
const stampSafe = handle.replace(/[^a-z0-9_-]/gi, "");
const candidateFile = join(archiveDir, `community-${stampSafe}-${Date.now()}.rs`);
writeFileSync(candidateFile, candidateSrc);

console.log(`scoring submission from ${handle} (replacing ${fnName} #${occurrence}, base ${task.base.slice(0, 8)})…`);
const r = sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", `cmd:cp ${candidateFile} ${task.evolveFile}`, "--json"]);
if (!r.ok) { console.error(`bench failed: ${r.out.slice(-400)}`); process.exit(1); }
const result = JSON.parse(r.out.slice(r.out.indexOf("{"))).results[0];

let incumbentScalar = null;
try {
  incumbentScalar = JSON.parse(readFileSync(resolve(repoRoot, "landing", "public", "arena.json"), "utf8")).incumbent.scalar;
} catch {}

const scalar = result.correctness === 1 ? result.metrics.scalar ?? 0 : 0;
console.log("\n--- verdict (paste-ready) ---");
if (result.gaming?.length) {
  console.log(`Gates: REJECTED (out of bounds: ${result.gaming.join("; ")})`);
} else if (result.correctness !== 1) {
  console.log(`Gates: FAILED (${result.error ? result.error.slice(0, 200) : "behavior diverged or tests failed"}). The machine says no.`);
} else {
  const vs = incumbentScalar ? ` Incumbent: ${incumbentScalar}x.` : "";
  const margin = incumbentScalar ? scalar - incumbentScalar : null;
  const call = margin === null ? "" : margin >= 0.005 ? " Clears the promotion margin — this ships, attributed." : margin > 0 ? " Above the incumbent, under the +0.005 promotion margin. Close." : margin === 0 ? " Exactly matches the incumbent." : " Behavior-identical, but more compute than the incumbent.";
  console.log(`Gates: PASSED (behavior bit-identical). Score: ${scalar}x.${vs}${call}`);
}
console.log(`Candidate archived: ${candidateFile}`);
