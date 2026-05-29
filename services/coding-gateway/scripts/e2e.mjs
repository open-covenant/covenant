// Manual end-to-end check: drives a real coding task through the backend +
// sandbox using the live ANTHROPIC_API_KEY, streams the agent working, then
// INDEPENDENTLY runs the produced program to confirm it actually works.
//
//   pnpm -C services/coding-gateway build
//   ANTHROPIC_API_KEY=... node services/coding-gateway/scripts/e2e.mjs
//   USE_E2B=1 E2B_API_KEY=... node ... scripts/e2e.mjs   # exercise the E2B path
import { AnthropicBackend } from "../dist/backends/anthropic.js";
import { LocalSandboxProvider } from "../dist/sandbox/local.js";

const task = `Create a Python script named rpn.py that evaluates a reverse-Polish-notation
expression given as a single command-line argument and prints only the numeric result.
Support + - * / on integers and floats. Example: \`python3 rpn.py '3 4 + 2 *'\` prints 14.
After writing it, run it on '3 4 + 2 *' and on '10 2 / 3 -' to confirm the outputs are 14 and 2.`;

let provider;
if (process.env.USE_E2B === "1") {
  const { E2bSandboxProvider } = await import("../dist/sandbox/e2b.js");
  provider = new E2bSandboxProvider(process.env.E2B_API_KEY);
  console.log("provider: E2B");
} else {
  provider = new LocalSandboxProvider();
  console.log("provider: local");
}

const sandbox = await provider.create({
  runId: "e2e-" + Date.now(),
  egressAllowlist: [],
  cpuMs: 600_000,
  memoryMb: 2048,
  diskMb: 5120,
  wallMs: 600_000,
});

const backend = new AnthropicBackend();
const ac = new AbortController();
const t0 = Date.now();

const { output, usage } = await backend.run({
  input: task,
  sandbox,
  signal: ac.signal,
  emit: (e) => {
    if (e.type === "tool.started") process.stdout.write(`\n  → ${e.tool}${e.preview ? ": " + e.preview : ""}`);
    else if (e.type === "tool.completed") process.stdout.write(` ${e.error ? "✗" : "✓"} (${e.duration_s.toFixed(1)}s)`);
    else if (e.type === "file.written") process.stdout.write(`\n  📝 ${e.path} (${e.bytes}b)`);
    else if (e.type === "run.failed") process.stdout.write(`\n  ✗ FAILED: ${e.error}`);
  },
});

console.log(`\n\n=== AGENT SUMMARY (${((Date.now() - t0) / 1000).toFixed(0)}s) ===\n${output}`);
console.log(`\n=== TOKENS === in=${usage.inputTokens} out=${usage.outputTokens} cacheRead=${usage.cacheReadTokens}`);

console.log(`\n=== INDEPENDENT VERIFY (I run its code myself) ===`);
let allOk = true;
for (const [expr, want] of [
  ["3 4 + 2 *", 14],
  ["10 2 / 3 -", 2],
  ["5 1 2 + 4 * + 3 -", 14],
]) {
  const r = await sandbox.exec(`python3 rpn.py '${expr}'`);
  const got = parseFloat(r.stdout.trim());
  const ok = Number.isFinite(got) && Math.abs(got - want) < 1e-9;
  if (!ok) allOk = false;
  console.log(`  '${expr}' → '${r.stdout.trim()}' (exit ${r.exitCode}) ${ok ? "✓" : "✗ want " + want}`);
}
console.log(`\nRESULT: ${allOk ? "PASS — produced working code" : "FAIL — code did not verify"}`);

await sandbox.destroy();
process.exit(allOk ? 0 : 1);
