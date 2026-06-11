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
const grokModel = opt("--grok-model", "grok-4.3");
const codexModel = opt("--codex-model", "gpt-5.5");
// 0.005 since 2026-06-10 (was 0.02): the fuel metric is deterministic, so
// any measured gain is real; the old margin systematically killed small-gain
// styles. Disclosed in docs/arena-challenge.md, applied prospectively.
const margin = parseFloat(opt("--margin", "0.005"));
const iters = parseInt(opt("--iters", "1"), 10);
const attempts = parseInt(opt("--attempts", "3"), 10);
const round = (n) => Math.round(n * 1000) / 1000;

function dotenv(name) {
  if (process.env[name]) return process.env[name];
  try {
    const line = readFileSync(join(repoRoot, ".env"), "utf8")
      .split("\n")
      .find((l) => l.startsWith(`${name}=`));
    return line ? line.slice(name.length + 1).trim() : null;
  } catch {
    return null;
  }
}
const xaiKey = dotenv("XAI_API_KEY");
const openaiKey = dotenv("OPENAI_API_KEY");

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

// Fast wasm compile-check sandbox: a standalone copy of the kernel crate.
// Both proposers get free fixups for extraction/compile failures (the CLI
// proposer can compile locally inside its agent loop; the API proposer
// cannot — this levels that without spending scored attempts).
const checkDir = join(archiveDir, "compile-check");
function compileCheck(candidateSrc) {
  mkdirSync(join(checkDir, "src"), { recursive: true });
  writeFileSync(join(checkDir, "Cargo.toml"), `[package]
name = "covenant-audit-kernel"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.11"

[workspace]
`);
  writeFileSync(join(checkDir, "src", "lib.rs"), candidateSrc);
  const r = sh("cargo", ["check", "--lib", "--target", "wasm32-wasip1", "--quiet"], { cwd: checkDir });
  return r.ok ? null : r.out.slice(-1500);
}

function splitEvolve(src) {
  const a = src.indexOf(START);
  const b = src.indexOf(END);
  if (a < 0 || b <= a) throw new Error("EVOLVE markers missing");
  return { pre: src.slice(0, a), block: src.slice(a, b + END.length), post: src.slice(b + END.length) };
}

function extractBlock(reply) {
  const a = reply.indexOf(START);
  const b = reply.lastIndexOf(END);
  if (a < 0 || b <= a) throw new Error(`proposer output is not a complete EVOLVE block: ${reply.trim().slice(0, 120)}…`);
  return reply.slice(a, b + END.length);
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

  const stamp = `k${ledger.versions.reduce((m, v) => Math.max(m, parseInt(v.version.match(/^k(\d+)/)?.[1] ?? 0, 10)), 0) + 1}`;

  const proposers = [
    {
      name: "grok",
      model: grokModel,
      // Multi-turn chat: feedback goes back as user turns.
      call: (messages, attempt) => {
        if (!xaiKey) throw new Error("XAI_API_KEY not set");
        const bodyFile = join(archiveDir, `${stamp}-grok-request-${attempt}.json`);
        writeFileSync(bodyFile, JSON.stringify({ model: grokModel, messages, max_tokens: 131072, reasoning_effort: "high" }));
        const r = sh("curl", ["-s", "-m", "1800", "-X", "POST", "https://api.x.ai/v1/chat/completions", "-H", `Authorization: Bearer ${xaiKey}`, "-H", "Content-Type: application/json", "--data", `@${bodyFile}`]);
        if (!r.ok) throw new Error(`xai request failed: ${r.out.slice(-200)}`);
        const d = JSON.parse(r.out);
        if (!d.choices) throw new Error(`xai error: ${JSON.stringify(d).slice(0, 300)}`);
        if (d.choices[0].finish_reason === "length") throw new Error("xai output truncated at max_tokens");
        return d.choices[0].message.content;
      },
    },
    {
      name: "codex",
      model: codexModel,
      call: (messages, attempt) => {
        if (!openaiKey) throw new Error("OPENAI_API_KEY not set");
        const bodyFile = join(archiveDir, `${stamp}-codex-request-${attempt}.json`);
        writeFileSync(bodyFile, JSON.stringify({ model: codexModel, messages, max_completion_tokens: 100000, reasoning_effort: "high" }));
        const r = sh("curl", ["-s", "-m", "1800", "-X", "POST", "https://api.openai.com/v1/chat/completions", "-H", `Authorization: Bearer ${openaiKey}`, "-H", "Content-Type: application/json", "--data", `@${bodyFile}`]);
        if (!r.ok) throw new Error(`openai request failed: ${r.out.slice(-200)}`);
        const d = JSON.parse(r.out);
        if (!d.choices) throw new Error(`openai error: ${JSON.stringify(d).slice(0, 300)}`);
        if (d.choices[0].finish_reason === "length") throw new Error("openai output truncated at max_completion_tokens");
        return d.choices[0].message.content;
      },
    },
    {
      name: "fable",
      model: proposerModel,
      // Empty scratch cwd: the CLI proposer is an agent with tools and will
      // otherwise edit the kernel in the live repo instead of printing the
      // block (observed round 10). stdout is its only channel back. The CLI
      // is stateless per call, so the transcript is folded into the prompt.
      call: (messages, attempt) => {
        const scratch = join(archiveDir, `${stamp}-fable-scratch`);
        mkdirSync(scratch, { recursive: true });
        const folded = messages
          .map((m) => (m.role === "user" ? m.content : `## Your previous attempt\n\n${m.content}`))
          .join("\n\n");
        const r = sh("claude", ["-p", folded, "--model", proposerModel, "--dangerously-skip-permissions"], { cwd: scratch });
        if (!r.ok) throw new Error(`claude proposer failed (attempt ${attempt}): ${r.out.slice(-400)}`);
        return r.out;
      },
    },
  ].filter((p) => (p.name !== "grok" || xaiKey) && (p.name !== "codex" || openaiKey));

  // v2 rules: every proposer gets up to `attempts` tries with gate/fuel
  // feedback between tries, stopping early once it holds a promotable
  // candidate. Same rules both sides; the bench stays the only judge.
  const entries = [];
  for (const p of proposers) {
    const label = `${stamp}:${p.name}`;
    const messages = [{ role: "user", content: prompt }];
    const tries = [];
    let bestResult = null;
    for (let attempt = 1; attempt <= attempts; attempt++) {
      let result;
      try {
        // Up to 2 free fixups per attempt for extraction/compile failures —
        // formatting and syntax deaths are noise, not signal.
        let candidateSrc = null;
        for (let fix = 0; fix <= 2; fix++) {
          const reply = p.call(messages, attempt);
          messages.push({ role: "assistant", content: reply });
          let block;
          try {
            block = extractBlock(reply);
          } catch (e) {
            if (fix === 2) throw e;
            messages.push({ role: "user", content: "That reply did not contain a complete EVOLVE block. Resend the entire block, from the START marker to the END marker, nothing else." });
            continue;
          }
          const src = pre + block + post;
          const err = compileCheck(src);
          if (err) {
            if (fix === 2) throw new Error(`does not compile after fixups: ${err.slice(0, 200)}`);
            messages.push({ role: "user", content: `Your block does not compile:\n${err}\nFix it and resend the entire EVOLVE block.` });
            continue;
          }
          candidateSrc = src;
          break;
        }
        const candidateFile = join(archiveDir, `${label.replace(":", "-")}-a${attempt}-candidate.rs`);
        writeFileSync(candidateFile, candidateSrc);
        const scored = benchScore(`cmd:cp ${candidateFile} ${task.evolveFile}`);
        const scalar = scored.correctness === 1 ? scored.metrics.scalar ?? 0 : 0;
        result = { candidateSrc, scored, scalar };
      } catch (e) {
        result = { scalar: 0, failure: String(e.message).slice(0, 200) };
        if (messages[messages.length - 1]?.role !== "assistant") messages.push({ role: "assistant", content: "(no usable block)" });
      }
      tries.push({ attempt, scalar: result.scalar, failure: result.failure ?? null });
      if (!bestResult || result.scalar > bestResult.scalar) bestResult = result;
      const gainNow = round(result.scalar - incumbent.metrics.scalar);
      console.log(`[${iter + 1}/${iters}] ${p.name} attempt ${attempt}/${attempts}: scalar=${result.scalar}${result.failure ? ` FAILED ${result.failure.slice(0, 100)}` : ""}`);
      if (result.scalar > 0 && gainNow >= margin) break;
      if (attempt < attempts) {
        const feedback = result.failure
          ? `That attempt failed before scoring: ${result.failure}. Output the complete EVOLVE block this time, nothing else.`
          : result.scored.correctness !== 1
            ? `Your block failed the gates (${result.scored.gaming?.length ? `gaming: ${result.scored.gaming.join("; ")}` : result.scored.error ? `error: ${result.scored.error.slice(0, 300)}` : "a correctness gate: unit, differential, sha, wasm-tests, or corpus digest"}). Fix correctness first, then optimize. Output the full corrected EVOLVE block.`
            : `Your block passed all gates and scored scalar ${result.scalar}; the incumbent is ${incumbent.metrics.scalar} and you need at least ${round(incumbent.metrics.scalar + margin)}. Find more fuel: look for remaining per-line allocations, schedule work, branchy scans, or redundant passes. Output the full improved EVOLVE block.`;
        messages.push({ role: "user", content: feedback });
      }
    }
    entries.push({ proposer: p.name, model: p.model, label, tries, ...bestResult });
  }

  const winner = entries.reduce((best, e) => (e.scalar > (best?.scalar ?? 0) ? e : best), null);
  const gain = round((winner?.scalar ?? 0) - incumbent.metrics.scalar);
  const promote = winner && winner.scored?.correctness === 1 && gain >= margin;

  for (const e of entries) {
    if (promote && e === winner) continue;
    const reason = e.failure ? `proposal failed: ${e.failure}`
      : e.scored?.gaming?.length ? `gaming: ${e.scored.gaming.join("; ")}`
      : e.scored?.error ? `error: ${e.scored.error}`
      : e.scored?.correctness !== 1 ? "correctness gate failed"
      : promote ? `lost tournament to ${winner.proposer} (${e.scalar} vs ${winner.scalar})`
      : `gain ${round(e.scalar - incumbent.metrics.scalar)} < margin ${margin}`;
    ledger.versions.push({ version: e.label, proposer: e.proposer, model: e.model, incumbent: incumbent.metrics.scalar, candidate: e.scalar, gain: round(e.scalar - incumbent.metrics.scalar), promoted: false, reason, tries: e.tries });
  }

  if (promote) {
    const candidateSrc = winner.candidateSrc;
    const candidate = winner.scored;
    writeFileSync(join(repoRoot, task.evolveFile), candidateSrc);
    const add = sh("git", ["-C", repoRoot, "add", task.evolveFile]);
    if (!add.ok) throw new Error(`git add failed: ${add.out}`);
    const commit = sh("git", [
      "-C", repoRoot,
      "-c", "user.name=Covenant",
      "-c", "user.email=covenant@users.noreply.github.com",
      "commit", "-m", `self-improvement(kernel): ${stamp} fuel scalar ${incumbent.metrics.scalar} -> ${candidate.metrics.scalar} (proposer ${winner.proposer})`,
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
    ledger.versions.push({ version: winner.label, proposer: winner.proposer, model: winner.model, incumbent: incumbent.metrics.scalar, candidate: candidate.metrics.scalar, gain, promoted: true, commit: sha, tries: winner.tries });
    console.log(`PROMOTED ${stamp} (${winner.proposer}): scalar ${incumbent.metrics.scalar} -> ${candidate.metrics.scalar} (commit ${sha.slice(0, 8)}, base advanced)`);
  } else {
    console.log(`REJECTED ${stamp}: no candidate beat the incumbent by the margin`);
  }
  writeFileSync(ledgerPath, JSON.stringify(ledger, null, 2));

  const arena = sh("node", [join(here, "gen-arena.mjs")]);
  if (arena.ok) {
    sh("git", ["-C", repoRoot, "add", "landing/public/arena.json"]);
    sh("git", [
      "-C", repoRoot,
      "-c", "user.name=Covenant",
      "-c", "user.email=covenant@users.noreply.github.com",
      "commit", "-m", `arena: round ${stamp.slice(1)} scoreboard`,
      "--only", "landing/public/arena.json",
    ]);
  } else {
    console.log(`arena update failed (non-fatal): ${arena.out.slice(-200)}`);
  }
}
