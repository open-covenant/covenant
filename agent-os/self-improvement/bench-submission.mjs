#!/usr/bin/env node
// Scores a community challenge submission: splices a replacement function
// into the incumbent kernel and runs it through the full gate stack in an
// isolated worktree. Score-only — promotion stays with the operator.
//
//   node bench-submission.mjs <submission.rs> [--handle @someone]
//     [--fn name] [--occurrence 1] [--block]
//
// Three submission shapes:
//   default: every `fn name` defined in the file replaces occurrence 1 of
//            the same-named item in the kernel (the wasm/metered path);
//   --fn:    replace exactly one named function (--occurrence to pick twins);
//   --block: the file is a complete EVOLVE block, spliced on the markers.
// Replacement items must be complete, including attributes
// (e.g. #[cfg(target_arch = "wasm32")]).

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
const fnName = opt("--fn", null);
const occurrence = parseInt(opt("--occurrence", "1"), 10);
const blockMode = argv.includes("--block");
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

const submission = readFileSync(file, "utf8").trim();

function spliceFn(source, name, item, occ) {
  const needle = new RegExp(`\\bfn\\s+${name}\\s*[(<]`, "g");
  const hits = [...source.matchAll(needle)];
  if (hits.length < occ) throw new Error(`found ${hits.length} definition(s) of ${name}, wanted occurrence ${occ}`);
  const fnIdx = hits[occ - 1].index;
  const start = itemStart(source, fnIdx);
  const end = itemEnd(source, source.indexOf("{", fnIdx));
  const indent = "    ";
  const indented = item.split("\n").map((l) => (l.trim() ? indent + l : l)).join("\n").replace(/^ +/, indent);
  return source.slice(0, start) + indented + "\n" + source.slice(end).replace(/^\n/, "");
}

// Split the submission file into top-level fn items (attrs + body).
function submissionItems(text) {
  const items = [];
  const needle = /(^|\n)\s*(#\[|\/\/\/|\/\/)?/;
  const fnRe = /\bfn\s+([A-Za-z0-9_]+)\s*[(<]/g;
  let m;
  const starts = [];
  while ((m = fnRe.exec(text))) starts.push({ name: m[1], idx: m.index });
  for (let i = 0; i < starts.length; i++) {
    const start = itemStart(text, starts[i].idx);
    const end = itemEnd(text, text.indexOf("{", starts[i].idx));
    // skip fns nested inside a previous item
    if (items.length && start < items[items.length - 1].end) continue;
    items.push({ name: starts[i].name, start, end });
  }
  return items.map((it) => ({ name: it.name, text: text.slice(it.start, it.end), end: it.end }));
}

let candidateSrc;
if (blockMode) {
  const START = "// EVOLVE-BLOCK-START";
  const END = "// EVOLVE-BLOCK-END";
  const sa = src.indexOf(START), sb = src.indexOf(END);
  const ba = submission.indexOf(START), bb = submission.lastIndexOf(END);
  if (ba < 0 || bb <= ba) { console.error("submission is not a complete EVOLVE block"); process.exit(1); }
  candidateSrc = src.slice(0, sa) + submission.slice(ba, bb + END.length) + src.slice(sb + END.length);
} else if (fnName) {
  if (!submission.includes(`fn ${fnName}`)) { console.error(`submission does not define fn ${fnName}`); process.exit(1); }
  candidateSrc = spliceFn(src, fnName, submission, occurrence);
} else {
  const items = submissionItems(submission);
  if (!items.length) { console.error("no fn definitions found in submission"); process.exit(1); }
  candidateSrc = src;
  for (const it of items) candidateSrc = spliceFn(candidateSrc, it.name, it.text, 1);
  console.log(`replacing: ${items.map((i) => i.name).join(", ")}`);
}

mkdirSync(archiveDir, { recursive: true });
const stampSafe = handle.replace(/[^a-z0-9_-]/gi, "");
const candidateFile = join(archiveDir, `community-${stampSafe}-${Date.now()}.rs`);
writeFileSync(candidateFile, candidateSrc);

console.log(`scoring submission from ${handle} (${blockMode ? "full block" : fnName ? `${fnName} #${occurrence}` : "auto-detected fns"}, base ${task.base.slice(0, 8)})…`);
const r = sh("node", [benchRun, "--task", "audit-kernel-fuel", "--solver", `cmd:cp ${candidateFile} ${task.evolveFile}`, "--json"]);
if (!r.ok) { console.error(`bench failed: ${r.out.slice(-400)}`); process.exit(1); }
const result = JSON.parse(r.out.slice(r.out.indexOf("{"))).results[0];

let incumbentScalar = null;
try {
  incumbentScalar = JSON.parse(readFileSync(resolve(repoRoot, "landing", "public", "arena.json"), "utf8")).incumbent.scalar;
} catch {}

const scalar = result.correctness === 1 ? result.metrics.scalar ?? 0 : 0;
const MARGIN = 0.005;
const round3 = (n) => Math.round(n * 1000) / 1000;

// Which gate stage failed, in plain English — so a rejection is a diagnosis,
// not a binary no. The bench runs stages in order; the first failure stops it.
function gateDiagnosis() {
  if (result.gaming?.length) return `diff confinement — your change touched something outside the allowed region (${result.gaming.join("; ")}). Edit only the kernel's EVOLVE block.`;
  const e = result.error ?? "";
  if (/compile|error\[|cannot find|mismatched types|expected/i.test(e)) return `it does not compile:\n${e.slice(-600)}`;
  if (/kernel_differential|differential/i.test(e)) return "the held-out differential suite — your output diverged from the frozen reference on an adversarial input the public corpus doesn't contain. Behavior must be bit-identical on every input, not just the common case.";
  if (/sha_differential/i.test(e)) return "the exhaustive sha differential — your hash output differs from the reference on some input length. Check padding/block-boundary handling.";
  if (/digest|corpus/i.test(e)) return "the frozen corpus digest — behavior drifted on the 50k-event corpus. Some event class now verifies differently.";
  if (/wasm-tests|wasm/i.test(e)) return "the wasm-executed test suite — your code behaves differently compiled to wasm than native (often a wasm-only #[cfg] path).";
  if (/unit|test result/i.test(e)) return "a unit test — a pinned invariant (exact sha hex, the prev\\nhash composition, or the zero root) changed.";
  return e ? `a correctness gate:\n${e.slice(-400)}` : "behavior diverged or a test failed.";
}

console.log("\n--- verdict (paste-ready) ---");
if (result.gaming?.length || result.correctness !== 1) {
  console.log(`Gates: REJECTED. ${gateDiagnosis()}`);
} else {
  const vs = incumbentScalar ? ` Incumbent ${incumbentScalar}x.` : "";
  const delta = incumbentScalar ? round3(scalar - incumbentScalar) : null;
  const fuelCutPct = round3((1 - 1 / scalar) * 100);
  let call = "";
  if (delta !== null) {
    if (delta >= MARGIN) call = ` Gain +${delta} clears the +${MARGIN} margin — this ships to production, attributed to you.`;
    else if (delta > 0) call = ` Gain +${delta}, just under the +${MARGIN} margin. You need +${round3(MARGIN - delta)} more — find one more allocation, branch, or redundant pass and resubmit.`;
    else if (delta === 0) call = " Exactly matches the incumbent — behavior-identical but no fuel saved.";
    else call = ` Behavior-identical, but ${round3(-delta)} scalar more expensive than the incumbent.`;
  }
  console.log(`Gates: PASSED — behavior bit-identical through the full stack (unit, differential, sha, wasm-executed, corpus digest). Score ${scalar}x (${fuelCutPct}% of the original compute eliminated).${vs}${call}`);
}
console.log(`\nCandidate archived: ${candidateFile}`);
console.log(`Run it yourself: node agent-os/self-improvement/bench-submission.mjs <your.rs> --handle <you>`);
