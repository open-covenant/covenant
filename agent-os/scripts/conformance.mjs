#!/usr/bin/env node
// Unified conformance runner for Covenant's golden-vector suites.
//
// Covenant freezes its public wire and record contracts as committed,
// byte-exact golden vectors that a third-party implementation can target
// without reading the Rust source. This runner is the single command that
// executes every such suite, so "are we still conformant?" has one answer.
//
//   node agent-os/scripts/conformance.mjs           run every suite (uses cargo)
//   node agent-os/scripts/conformance.mjs --check    static integrity, no Rust tooling
//   node agent-os/scripts/conformance.mjs --list      print the suite manifest
//
// The --check mode is what `validate.sh --scripts` runs: it discovers the
// golden suites on disk and fails if any is not registered here, so a new
// suite can never be added without the runner learning about it (the
// "passes while skipping a suite silently" failure mode). The default mode
// additionally requires every suite to execute at least one test and report
// zero failures, so a renamed or deleted test surfaces as a hard error
// instead of a silently shrinking run.

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { homedir } from "node:os";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const AGENT_OS = join(SCRIPT_DIR, "..");
const CRATES = join(AGENT_OS, "crates");

// Every conformance suite. An integration suite is a `tests/<target>.rs`
// drift runner; an inline suite is a `#[test]` living in the crate's
// `src` (used where the contract needs the crate's private internals).
// `fixtures` are the committed golden artifacts the suite freezes; --check
// asserts each path exists so a moved or deleted corpus is caught early.
const SUITES = [
  {
    id: "ipc-requests",
    crate: "covenant-ipc",
    target: "golden_requests",
    description: "Every daemon IPC Request variant — one frozen wire vector each.",
    fixtures: ["tests/golden/requests"],
  },
  {
    id: "ipc-responses",
    crate: "covenant-ipc",
    target: "golden_responses",
    description:
      "Every daemon IPC Response variant plus the protocol-info and v2 streaming envelopes.",
    fixtures: ["tests/golden/responses", "tests/fixtures/v2", "tests/fixtures/protocol-info.v1.json"],
  },
  {
    id: "capability-grammar",
    crate: "covenant-permissions",
    target: "golden_capabilities",
    description: "One Capability grant per scope namespace — the capability action grammar.",
    fixtures: ["tests/golden/capabilities"],
  },
  {
    id: "audit-record-kinds",
    crate: "covenant-audit",
    target: "golden_audit_kinds",
    description: "Every AuditKind variant's record wire form and SHA-256 event hash.",
    fixtures: ["tests/fixtures/audit-kinds.v1.json"],
  },
  {
    id: "audit-provenance-chain",
    crate: "covenant-audit",
    inlineTest: "provenance_record_schema_golden_vectors_are_frozen",
    source: "src/lib.rs",
    description:
      "The capability-family provenance records and the genesis-seeded chain root fold.",
    fixtures: ["tests/fixtures/provenance-records.v1.json"],
  },
];

function cargoBin() {
  if (process.env.CARGO) return process.env.CARGO;
  const local = join(homedir(), ".cargo", "bin", "cargo");
  return existsSync(local) ? local : "cargo";
}

// Discover the integration golden runners on disk: crates/*/tests/golden_*.rs.
function discoverIntegrationTargets() {
  const found = [];
  for (const crate of readdirSync(CRATES, { withFileTypes: true })) {
    if (!crate.isDirectory()) continue;
    const testsDir = join(CRATES, crate.name, "tests");
    if (!existsSync(testsDir)) continue;
    for (const entry of readdirSync(testsDir, { withFileTypes: true })) {
      if (!entry.isFile()) continue;
      if (entry.name.startsWith("golden_") && entry.name.endsWith(".rs")) {
        found.push({ crate: crate.name, target: entry.name.slice(0, -3) });
      }
    }
  }
  return found;
}

function readRustFilesRecursive(dir) {
  const out = [];
  const stack = [dir];
  while (stack.length) {
    let entries;
    try {
      entries = readdirSync(stack.pop(), { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const path = join(entry.parentPath ?? entry.path, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else if (entry.isFile() && entry.name.endsWith(".rs")) out.push(path);
    }
  }
  return out;
}

// Discover inline golden suites by naming convention: a `fn
// *_golden_vectors_are_frozen` living in a crate's src (used where the contract
// needs the crate's private internals, like the audit chain fold). This keeps
// the silent-skip guarantee symmetric with the integration runners — a new
// inline suite cannot be added without registering it here.
function discoverInlineGoldenSuites() {
  const found = [];
  const re = /\bfn\s+(\w*golden_vectors_are_frozen)\s*\(/g;
  for (const crate of readdirSync(CRATES, { withFileTypes: true })) {
    if (!crate.isDirectory()) continue;
    const srcDir = join(CRATES, crate.name, "src");
    if (!existsSync(srcDir)) continue;
    for (const file of readRustFilesRecursive(srcDir)) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(re)) {
        found.push({ crate: crate.name, fn: match[1] });
      }
    }
  }
  return found;
}

// Static integrity: the runner's manifest must match reality. Returns a list
// of human-readable problems; empty means the manifest is sound.
function checkIntegrity() {
  const problems = [];

  const ids = new Set();
  for (const suite of SUITES) {
    if (ids.has(suite.id)) problems.push(`duplicate suite id: ${suite.id}`);
    ids.add(suite.id);
    if (!suite.target && !suite.inlineTest) {
      problems.push(`suite ${suite.id} declares neither a target nor an inlineTest`);
    }
  }

  // Every on-disk golden_*.rs runner must be a registered integration suite,
  // and every registered integration suite must exist on disk.
  const onDisk = discoverIntegrationTargets();
  const onDiskKeys = new Set(onDisk.map((t) => `${t.crate}/${t.target}`));
  const manifestKeys = new Set(
    SUITES.filter((s) => s.target).map((s) => `${s.crate}/${s.target}`),
  );
  for (const key of onDiskKeys) {
    if (!manifestKeys.has(key)) {
      problems.push(
        `unregistered golden suite on disk: crates/${key}.rs — add it to SUITES in conformance.mjs`,
      );
    }
  }
  for (const key of manifestKeys) {
    if (!onDiskKeys.has(key)) {
      problems.push(`manifest references a missing golden runner: crates/${key}.rs`);
    }
  }

  // Every inline golden suite found in src by convention must be registered.
  const inlineManifest = new Set(
    SUITES.filter((s) => s.inlineTest).map((s) => `${s.crate}/${s.inlineTest}`),
  );
  for (const { crate, fn } of discoverInlineGoldenSuites()) {
    if (!inlineManifest.has(`${crate}/${fn}`)) {
      problems.push(
        `unregistered inline golden suite: ${crate} fn ${fn} — add it to SUITES in conformance.mjs`,
      );
    }
  }

  // Inline suites must still exist as a named test in their source file.
  for (const suite of SUITES) {
    if (!suite.inlineTest) continue;
    const src = join(CRATES, suite.crate, suite.source);
    if (!existsSync(src)) {
      problems.push(`suite ${suite.id}: source ${suite.crate}/${suite.source} not found`);
      continue;
    }
    if (!readFileSync(src, "utf8").includes(`fn ${suite.inlineTest}`)) {
      problems.push(
        `suite ${suite.id}: inline test fn ${suite.inlineTest} not found in ${suite.crate}/${suite.source}`,
      );
    }
  }

  // Declared fixtures must exist.
  for (const suite of SUITES) {
    for (const rel of suite.fixtures ?? []) {
      const path = join(CRATES, suite.crate, rel);
      if (!existsSync(path)) {
        problems.push(`suite ${suite.id}: missing fixture ${suite.crate}/${rel}`);
      } else if (statSync(path).isDirectory() && readdirSync(path).length === 0) {
        problems.push(`suite ${suite.id}: fixture directory ${suite.crate}/${rel} is empty`);
      }
    }
  }

  return problems;
}

function cargoArgs(suite) {
  const base = ["test", "-p", suite.crate, "--locked"];
  if (suite.target) return [...base, "--test", suite.target];
  return [...base, "--lib", suite.inlineTest];
}

// Sum `N passed` / `M failed` across every "test result:" line cargo prints
// (one per test binary). Returns null if no result line was seen at all.
function parseCargoResult(output) {
  const re = /test result: \w+\. (\d+) passed; (\d+) failed/g;
  let match;
  let seen = false;
  let passed = 0;
  let failed = 0;
  while ((match = re.exec(output)) !== null) {
    seen = true;
    passed += Number(match[1]);
    failed += Number(match[2]);
  }
  return seen ? { passed, failed } : null;
}

function runSuite(suite) {
  const args = cargoArgs(suite);
  const proc = spawnSync(cargoBin(), args, {
    cwd: AGENT_OS,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (proc.error) {
    const hint =
      proc.error.code === "ENOENT"
        ? ` — '${cargoBin()}' not found; set CARGO or install the Rust toolchain`
        : "";
    return { ok: false, passed: 0, reason: `failed to run cargo${hint}`, output: String(proc.error) };
  }
  const output = `${proc.stdout ?? ""}${proc.stderr ?? ""}`;
  const result = parseCargoResult(output);

  if (proc.status !== 0) {
    return { ok: false, passed: result?.passed ?? 0, reason: "cargo reported failures", output };
  }
  if (!result) {
    return { ok: false, passed: 0, reason: "no test result line — did the suite compile?", output };
  }
  if (result.failed > 0) {
    return { ok: false, passed: result.passed, reason: `${result.failed} failed`, output };
  }
  if (result.passed === 0) {
    return {
      ok: false,
      passed: 0,
      reason: "ran zero tests — a renamed or deleted suite would skip silently",
      output,
    };
  }
  return { ok: true, passed: result.passed, reason: null, output };
}

function main() {
  const args = process.argv.slice(2);
  const unknown = args.filter((a) => !["--check", "--list"].includes(a));
  if (unknown.length) {
    process.stderr.write(`conformance: unknown argument(s): ${unknown.join(", ")}\n`);
    process.stderr.write("usage: conformance.mjs [--check | --list]\n");
    process.exit(2);
  }

  if (args.includes("--list")) {
    for (const suite of SUITES) {
      const how = suite.target ? `tests/${suite.target}.rs` : `src inline: ${suite.inlineTest}`;
      process.stdout.write(`${suite.id}\t${suite.crate}\t${how}\n    ${suite.description}\n`);
    }
    return;
  }

  // Static integrity always runs first — there is no point executing a stale
  // manifest, and it is the only check --check performs.
  const problems = checkIntegrity();
  if (problems.length) {
    process.stderr.write("conformance: manifest integrity FAILED\n");
    for (const p of problems) process.stderr.write(`  - ${p}\n`);
    process.exit(1);
  }

  if (args.includes("--check")) {
    process.stdout.write(
      `conformance: ok (${SUITES.length} suites registered, all on-disk runners and fixtures present)\n`,
    );
    return;
  }

  process.stdout.write(`conformance: running ${SUITES.length} golden-vector suites via cargo\n`);
  let failures = 0;
  let totalPassed = 0;
  for (const suite of SUITES) {
    const result = runSuite(suite);
    totalPassed += result.passed;
    if (result.ok) {
      process.stdout.write(`  ok    ${suite.id} (${result.passed} tests)\n`);
    } else {
      failures += 1;
      process.stdout.write(`  FAIL  ${suite.id}: ${result.reason}\n`);
      process.stderr.write(result.output);
    }
  }

  if (failures) {
    process.stderr.write(`conformance: ${failures} of ${SUITES.length} suites FAILED\n`);
    process.exit(1);
  }
  process.stdout.write(
    `conformance: ok (${SUITES.length} suites, ${totalPassed} frozen-contract tests passed)\n`,
  );
}

main();
