#!/usr/bin/env node
// Verify the Kani proof harnesses two ways: mechanically (did every harness
// pass) and adversarially (a panel of independent LLM reviewers each tries to
// REFUTE the proofs). The adversarial pass catches the failure Kani can't
// report: a harness that passes only because an over-tight `kani::assume` made
// it vacuously true, or an assertion that doesn't encode what its name claims.
// One agent rubber-stamping its own read is the weak spot; three independent
// skeptics attacking from different angles is not.
//
// The canonical harness set is parsed from the source, not from what the
// reviewers happen to name, so a misspelled or hallucinated name can't split
// the vote or hide a harness. A harness is rejected when >= MAJORITY of
// reviewers refute it, and the run is inconclusive (fail-safe) if any harness
// wasn't covered by a majority. Reviewers run through `claude -p` (headless,
// the local Claude subscription), not a billed API key. CI runs Kani alone;
// this script is the local / loop gate.
//
//   node scripts/verify-proofs.mjs                 # Kani + adversarial panel
//   node scripts/verify-proofs.mjs --no-llm        # mechanical only
//   node scripts/verify-proofs.mjs --kani-output f # reuse a prior Kani run
//   node scripts/verify-proofs.mjs --selftest      # check the parsing helpers
//
// Exit 0 only when Kani passes AND every harness is covered and survives.
// Exit 1 on a verification verdict of fail; exit 2 on a tooling/setup error.

import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const AGENT_OS = join(dirname(fileURLToPath(import.meta.url)), "..");
const CRATE = "covenant-budget";
const PROOF_SRC = join(AGENT_OS, "crates", CRATE, "src", "lib.rs");
const MODEL = "claude-opus-4-8";
const CLAUDE_TIMEOUT_MS = 300_000; // one reviewer call; generous under contention
const KANI_TIMEOUT_MS = 900_000; // a single intractable harness must not wedge the gate

// Independent adversarial reviewers, each attacking from a different angle. A
// harness is rejected when >= MAJORITY of them refute it: resilient to one
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
// Used only to feed context to the reviewers; the authoritative harness list
// comes from harnessNames(), and coverage checks catch a bad slice.
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

// Authoritative harness list: the `fn` name following each #[kani::proof],
// tolerant of intervening attributes like #[kani::unwind(..)].
function harnessNames(src) {
  const names = [];
  const re = /#\[kani::proof\][\s\S]*?\bfn\s+([A-Za-z_]\w*)/g;
  for (let m; (m = re.exec(src)); ) names.push(m[1]);
  return names;
}

// Reviewers may report `proofs::name`, `` `name` ``, or "name ": reduce to the
// bare trailing identifier so it maps onto the canonical set.
function normName(s) {
  return (
    String(s)
      .replace(/[`'"]/g, "")
      .trim()
      .split("::")
      .pop()
      .match(/[A-Za-z_]\w*/)?.[0] ?? ""
  );
}

function runKani() {
  const r = spawnSync("cargo", ["kani", "--package", CRATE, "--output-format", "terse"], {
    cwd: AGENT_OS,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: KANI_TIMEOUT_MS,
    killSignal: "SIGKILL",
  });
  if (r.error?.code === "ENOENT") {
    die("`cargo kani` not found. Install: cargo install --locked kani-verifier && cargo kani setup");
  }
  if (r.error?.code === "ETIMEDOUT") {
    die(`cargo kani exceeded ${KANI_TIMEOUT_MS / 1000}s and was killed (intractable harness?)`);
  }
  if (r.error) die(`cargo kani failed to run: ${r.error.message}`);
  return (r.stdout || "") + (r.stderr || "");
}

// Kani's terse footer: "Complete - N successfully verified harnesses, M failures, T total."
function mechanicalVerdict(out) {
  const m = out.match(/Complete - (\d+) successfully verified harnesses?, (\d+) failures?, (\d+) total/);
  if (!m) return { pass: false, reason: "no Kani summary line found", verified: 0, failures: 0, total: 0 };
  const [, verified, failures, total] = m.map(Number);
  return { pass: failures === 0 && total > 0, verified, failures, total };
}

// First balanced {...} object, string-aware so braces inside JSON string values
// don't close it early. Tolerates a ```json fence and surrounding prose.
function firstJsonObject(text) {
  const fence = text.match(/```(?:json)?\s*([\s\S]*?)```/);
  const body = fence ? fence[1] : text;
  const start = body.indexOf("{");
  if (start < 0) return null;
  let depth = 0;
  let inStr = false;
  let esc = false;
  for (let i = start; i < body.length; i++) {
    const ch = body[i];
    if (inStr) {
      if (esc) esc = false;
      else if (ch === "\\") esc = true;
      else if (ch === '"') inStr = false;
    } else if (ch === '"') inStr = true;
    else if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) return body.slice(start, i + 1);
  }
  return null;
}

function claude(prompt) {
  return new Promise((resolve, reject) => {
    const child = spawn("claude", ["-p", "--output-format", "json", "--model", MODEL], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    let out = "";
    let err = "";
    let done = false;
    const finish = (fn, arg) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      fn(arg);
    };
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      finish(reject, new Error(`claude timed out after ${CLAUDE_TIMEOUT_MS / 1000}s`));
    }, CLAUDE_TIMEOUT_MS);

    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("error", (e) =>
      finish(reject, e.code === "ENOENT" ? new Error("`claude` CLI not found") : e),
    );
    child.on("close", (code) => {
      if (code !== 0) return finish(reject, new Error(`claude exited ${code}: ${err.trim()}`));
      let env;
      try {
        env = JSON.parse(out);
      } catch {
        return finish(reject, new Error("unparseable claude --output-format json envelope"));
      }
      if (env.is_error) {
        return finish(reject, new Error(`claude returned ${env.subtype ?? "an error"}`));
      }
      const json = firstJsonObject(env.result ?? "");
      if (!json) return finish(reject, new Error(`no JSON verdict in model output:\n${env.result ?? ""}`));
      try {
        finish(resolve, JSON.parse(json));
      } catch {
        finish(reject, new Error(`model verdict was not valid JSON:\n${json}`));
      }
    });
    child.stdin.end(prompt);
  });
}

function reviewerPrompt(reviewer, fnContext, proofModule, kaniOut) {
  return `You are an adversarial formal-verification reviewer. Your job is to REFUTE the \
Kani proof harnesses below: prove they are worthless, not to wave them through. \
Attack each \`#[kani::proof]\` harness specifically along this axis:

${reviewer.lens}

Set refuted=true for a harness ONLY when you can state a concrete, correct flaw \
(vacuous assumptions, unreachable/tautological assertion, mis-encoded or \
too-weak property, unsound scope). Do NOT invent flaws to look thorough. If a \
harness is genuinely sound and meaningful under your axis, set refuted=false and \
say briefly why it holds up.

Respond with ONLY a JSON object, no prose, no markdown fence. Use the exact \
harness function name (no module prefix) and include one entry per harness:
{"reviews":[{"name":string,"refuted":boolean,"reason":string}]}

=== FUNCTIONS UNDER TEST ===
${fnContext}

=== PROOF HARNESSES ===
${proofModule}

=== KANI OUTPUT ===
${kaniOut}`;
}

async function adversarialPanel(canonical, fnContext, proofModule, kaniOut) {
  const settled = await Promise.allSettled(
    REVIEWERS.map((r) => claude(reviewerPrompt(r, fnContext, proofModule, kaniOut))),
  );
  const panel = settled.map((s, i) => ({
    id: REVIEWERS[i].id,
    ok: s.status === "fulfilled",
    reviews: s.status === "fulfilled" ? (s.value.reviews ?? []) : [],
    error: s.status === "rejected" ? String(s.reason?.message ?? s.reason) : null,
  }));

  const ok = panel.filter((p) => p.ok);
  for (const p of panel.filter((x) => !x.ok)) console.error(`  reviewer ${p.id} failed: ${p.error}`);
  if (ok.length < MAJORITY) {
    die(`only ${ok.length}/${REVIEWERS.length} reviewers responded; need ${MAJORITY} for a verdict`);
  }

  // Surface names a reviewer raised that aren't real harnesses (hallucination
  // or drift) so they can't silently affect the tally.
  const canon = new Set(canonical);
  for (const p of ok) {
    for (const r of p.reviews) {
      if (!canon.has(normName(r.name))) {
        console.error(`  note: reviewer ${p.id} referenced unknown harness "${r.name}"`);
      }
    }
  }

  // Tally each REAL harness across the reviewers that addressed it.
  const harnesses = canonical.map((name) => {
    const verdicts = ok
      .map((p) => ({ id: p.id, r: p.reviews.find((x) => normName(x.name) === name) }))
      .filter((x) => x.r);
    const refuters = verdicts.filter((x) => x.r.refuted);
    return {
      name,
      refuters,
      covered: verdicts.length,
      rejected: refuters.length >= MAJORITY,
      inconclusive: verdicts.length < MAJORITY,
    };
  });
  return { panel, harnesses };
}

// Deterministic checks of the parsing/aggregation primitives that the live LLM
// run can't exercise (real reviewers return clean names). Run: `--selftest`.
function selfTest() {
  assert.deepEqual(
    harnessNames("#[kani::proof]\n    fn alpha() {}\n#[kani::unwind(2)]\n#[kani::proof]\n    fn beta() {}"),
    ["alpha", "beta"],
  );
  assert.equal(normName("proofs::refill_never_panics"), "refill_never_panics");
  assert.equal(normName("`refill_clock_never_rewinds`"), "refill_clock_never_rewinds");
  assert.equal(normName("  project_overshoot_matches_spec "), "project_overshoot_matches_spec");
  // brace inside a string value must not close the object early
  assert.equal(firstJsonObject('noise ```json\n{"reason":"a } b","refuted":false}\n``` tail'), '{"reason":"a } b","refuted":false}');
  assert.equal(firstJsonObject("no json here"), null);
  const m = mechanicalVerdict("Manual Harness Summary:\nComplete - 5 successfully verified harnesses, 0 failures, 5 total.");
  assert.equal(m.pass, true);
  assert.equal(m.total, 5);
  assert.equal(mechanicalVerdict("Complete - 3 successfully verified harnesses, 1 failures, 4 total.").pass, false);
  console.log("selftest: ok");
}
if (args.includes("--selftest")) {
  selfTest();
  process.exit(0);
}

const src = readFileSync(PROOF_SRC, "utf8");
const canonical = harnessNames(src);
if (canonical.length === 0) die(`no #[kani::proof] harnesses found in ${PROOF_SRC}`);
const proofModule = extractProofModule(src);
const fnContext = extractFnContext(src);
const kaniOut = kaniOutputFile ? readFileSync(kaniOutputFile, "utf8") : runKani();

const mech = mechanicalVerdict(kaniOut);
console.log(`\nKani: ${mech.verified}/${mech.total} verified, ${mech.failures} failures`);
if (!mech.pass) die(`mechanical check failed (${mech.reason ?? "harness failure"})`, 1);
if (mech.total !== canonical.length) {
  die(`Kani ran ${mech.total} harnesses but the source declares ${canonical.length}, out of sync`, 1);
}

if (noLlm) {
  console.log("mechanical check passed (--no-llm; skipped adversarial review)\n");
  process.exit(0);
}

const { panel, harnesses } = await adversarialPanel(canonical, fnContext, proofModule, kaniOut);
const present = panel.filter((p) => p.ok).map((p) => p.id).join(", ");
console.log(`\nAdversarial panel (${present}), reject at ${MAJORITY}/${REVIEWERS.length} refutations:`);
for (const h of harnesses) {
  const tag = h.rejected ? "REJECT" : h.inconclusive ? "INCONC" : "ok    ";
  const reasons = h.refuters.map((x) => `${x.id}: ${x.r.reason}`).join(" | ");
  const detail = h.rejected
    ? ` (${h.refuters.length} refuted: ${reasons})`
    : h.inconclusive
      ? ` (only ${h.covered}/${REVIEWERS.length} reviewers covered it)`
      : reasons
        ? ` (${h.refuters.length} dissent: ${reasons})`
        : "";
  console.log(`  ${tag} ${h.name}${detail}`);
}

const bad = harnesses.filter((h) => h.rejected || h.inconclusive);
console.log(
  `\nverdict: ${bad.length ? "fail" : "pass"}: ${harnesses.length} harnesses, ` +
    `${harnesses.filter((h) => h.rejected).length} rejected, ` +
    `${harnesses.filter((h) => h.inconclusive).length} inconclusive\n`,
);
process.exit(bad.length ? 1 : 0);
