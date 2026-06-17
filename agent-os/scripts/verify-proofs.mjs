#!/usr/bin/env node
// Verify the Kani proof harnesses two ways: mechanically (did every harness
// pass) and adversarially (a panel of independent LLM reviewers each tries to
// REFUTE the proofs). The adversarial pass catches the failure Kani can't
// report — a harness that passes only because an over-tight `kani::assume`
// made it vacuously true, or an assertion that doesn't encode what its name
// claims. One agent rubber-stamping its own read is the weak spot; three
// independent skeptics attacking from different angles is not.
//
// Each harness survives only if FEWER THAN A MAJORITY of reviewers refute it.
// Reviewers run through `claude -p` (headless, the local Claude subscription),
// not a billed API key. CI runs Kani alone; this script is the local / loop
// gate.
//
//   node scripts/verify-proofs.mjs                 # Kani + adversarial panel
//   node scripts/verify-proofs.mjs --no-llm        # mechanical only
//   node scripts/verify-proofs.mjs --kani-output f # reuse a prior Kani run
//
// Exit 0 only when the mechanical check passes AND every harness survives.

import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const AGENT_OS = join(dirname(fileURLToPath(import.meta.url)), "..");
const CRATE = "covenant-budget";
const PROOF_SRC = join(AGENT_OS, "crates", CRATE, "src", "lib.rs");
const MODEL = "claude-opus-4-8";

// Independent adversarial reviewers, each attacking from a different angle. A
// harness is rejected when >= MAJORITY of them refute it — resilient to one
// over-aggressive reviewer, while any single genuine flaw is still surfaced.
const REVIEWERS = [
  {
    id: "vacuity",
    lens: "VACUITY and REACHABILITY. Do the kani::assume constraints (or the cfg setup) exclude the inputs that would expose a bug, leaving the assertion trivially true? Do any assertions report as UNREACHABLE in the Kani output, meaning they never actually executed? A proof of an unreachable or assumed-away property proves nothing.",
  },
  {
    id: "encoding",
    lens: "ENCODING STRENGTH. Does each assertion actually express the invariant its name and comment claim, or something strictly weaker or tautological? Would the assertion still pass if the function under test were subtly wrong? A no-panic harness that asserts nothing about the result is weak; say so.",
  },
  {
    id: "soundness",
    lens: "SOUNDNESS and SCOPE. Is the proven input domain the real operating domain, or are meaningful edge inputs (overflow boundaries, zero, max) silently excluded? Is panic-freedom asserting away a condition that SHOULD be treated as a failure? Could the harness pass for a reason unrelated to the claimed property?",
  },
];
const MAJORITY = Math.floor(REVIEWERS.length / 2) + 1;

const args = process.argv.slice(2);
const noLlm = args.includes("--no-llm");
const koIdx = args.indexOf("--kani-output");
const kaniOutputFile = koIdx >= 0 ? args[koIdx + 1] : null;

function die(msg, code = 2) {
  console.error(`verify-proofs: ${msg}`);
  process.exit(code);
}

// Slice [from .. matching close brace of the block that starts at `anchor`].
function blockFrom(src, from, anchor) {
  const a = src.indexOf(anchor, from);
  if (a < 0) return null;
  let depth = 0;
  for (let i = src.indexOf("{", a); i < src.length; i++) {
    if (src[i] === "{") depth++;
    else if (src[i] === "}" && --depth === 0) return { start: from, end: i + 1 };
  }
  return null;
}

function extractProofModule(src) {
  const b = blockFrom(src, src.lastIndexOf("#[cfg(kani)]"), "mod proofs {");
  if (!b) die(`no \`mod proofs\` block in ${PROOF_SRC}`);
  return src.slice(b.start, b.end);
}

// The functions under test, so reviewers can judge whether assertions match
// the real code rather than just the harness's own comments.
function extractFnContext(src) {
  const from = src.indexOf("const MS_PER_HOUR");
  const b = blockFrom(src, from, "fn refill_eta_ms");
  if (from < 0 || !b) die("could not locate budget arithmetic in lib.rs");
  return src.slice(from, b.end);
}

function runKani() {
  const r = spawnSync(
    "cargo",
    ["kani", "--package", CRATE, "--output-format", "terse"],
    { cwd: AGENT_OS, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  if (r.error?.code === "ENOENT") {
    die("`cargo kani` not found. Install: cargo install --locked kani-verifier && cargo kani setup");
  }
  return (r.stdout || "") + (r.stderr || "");
}

// Kani's terse footer: "Complete - N successfully verified harnesses, M failures, T total."
function mechanicalVerdict(out) {
  const m = out.match(/Complete - (\d+) successfully verified harnesses?, (\d+) failures?, (\d+) total/);
  if (!m) return { pass: false, reason: "no Kani summary line found", verified: 0, failures: 0, total: 0 };
  const [, verified, failures, total] = m.map(Number);
  return { pass: failures === 0 && total > 0, verified, failures, total };
}

function claude(prompt) {
  return new Promise((resolve, reject) => {
    const child = spawn("claude", ["-p", "--output-format", "json", "--model", MODEL], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("error", (e) => reject(e.code === "ENOENT" ? new Error("`claude` CLI not found") : e));
    child.on("close", (code) => {
      if (code !== 0) return reject(new Error(`claude exited ${code}: ${err.trim()}`));
      let env;
      try {
        env = JSON.parse(out);
      } catch {
        return reject(new Error("unparseable claude json envelope"));
      }
      const text = env.result ?? "";
      const o = text.indexOf("{");
      const c = text.lastIndexOf("}");
      if (o < 0 || c < 0) return reject(new Error(`no JSON verdict in output:\n${text}`));
      try {
        resolve(JSON.parse(text.slice(o, c + 1)));
      } catch {
        reject(new Error(`verdict not valid JSON:\n${text.slice(o, c + 1)}`));
      }
    });
    child.stdin.end(prompt);
  });
}

function reviewerPrompt(reviewer, fnContext, proofModule, kaniOut) {
  return `You are an adversarial formal-verification reviewer. Your job is to REFUTE the \
Kani proof harnesses below — prove they are worthless, not to wave them through. \
Attack each \`#[kani::proof]\` harness specifically along this axis:

${reviewer.lens}

Set refuted=true for a harness ONLY when you can state a concrete, correct flaw \
(vacuous assumptions, unreachable/tautological assertion, mis-encoded or \
too-weak property, unsound scope). Do NOT invent flaws to look thorough — if a \
harness is genuinely sound and meaningful under your axis, set refuted=false and \
say briefly why it holds up.

Respond with ONLY a JSON object, no prose, no markdown fence:
{"reviews":[{"name":string,"refuted":boolean,"reason":string}]}
Include one entry per harness.

=== FUNCTIONS UNDER TEST ===
${fnContext}

=== PROOF HARNESSES ===
${proofModule}

=== KANI OUTPUT ===
${kaniOut}`;
}

async function adversarialPanel(fnContext, proofModule, kaniOut) {
  const settled = await Promise.allSettled(
    REVIEWERS.map((r) => claude(reviewerPrompt(r, fnContext, proofModule, kaniOut))),
  );

  const panel = settled.map((s, i) => ({
    id: REVIEWERS[i].id,
    ok: s.status === "fulfilled",
    reviews: s.status === "fulfilled" ? s.value.reviews ?? [] : [],
    error: s.status === "rejected" ? String(s.reason?.message ?? s.reason) : null,
  }));

  const ok = panel.filter((p) => p.ok);
  if (ok.length < MAJORITY) {
    for (const p of panel.filter((x) => !x.ok)) console.error(`  reviewer ${p.id} failed: ${p.error}`);
    die(`only ${ok.length}/${REVIEWERS.length} reviewers responded; need ${MAJORITY} for a verdict`);
  }

  const names = new Set();
  for (const p of ok) for (const r of p.reviews) names.add(r.name);

  const harnesses = [...names].map((name) => {
    const refuters = ok
      .map((p) => ({ id: p.id, r: p.reviews.find((x) => x.name === name) }))
      .filter((x) => x.r?.refuted);
    return { name, refuters, rejected: refuters.length >= MAJORITY };
  });

  return { panel, harnesses };
}

const src = readFileSync(PROOF_SRC, "utf8");
const proofModule = extractProofModule(src);
const fnContext = extractFnContext(src);
const kaniOut = kaniOutputFile ? readFileSync(kaniOutputFile, "utf8") : runKani();

const mech = mechanicalVerdict(kaniOut);
console.log(`\nKani: ${mech.verified}/${mech.total} verified, ${mech.failures} failures`);
if (!mech.pass) die(`mechanical check failed (${mech.reason ?? "harness failure"})`, 1);

if (noLlm) {
  console.log("mechanical check passed (--no-llm; skipped adversarial review)\n");
  process.exit(0);
}

const { panel, harnesses } = await adversarialPanel(fnContext, proofModule, kaniOut);
console.log(`\nAdversarial panel (${panel.filter((p) => p.ok).map((p) => p.id).join(", ")}) — reject at ${MAJORITY}/${REVIEWERS.length} refutations:`);
for (const h of harnesses) {
  const tag = h.rejected ? "REJECT" : "ok    ";
  const detail = h.refuters.length
    ? ` (${h.refuters.length} refuted: ${h.refuters.map((x) => `${x.id}: ${x.r.reason}`).join(" | ")})`
    : "";
  console.log(`  ${tag} ${h.name}${detail}`);
}

const rejected = harnesses.filter((h) => h.rejected);
console.log(`\nverdict: ${rejected.length ? "fail" : "pass"} — ${harnesses.length} harnesses, ${rejected.length} rejected by the panel\n`);
process.exit(rejected.length ? 1 : 0);
