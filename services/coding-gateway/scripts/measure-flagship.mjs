// Instrumented flagship probe: drives the real heavy build through the
// backend + E2B sandbox with a heartbeat, full event logging, and a
// wall-clock abort, so we can see first-token latency and whether the run
// streams at all (vs. hangs) and how long a real build takes end-to-end.
import { AnthropicBackend } from "../dist/backends/anthropic.js";

const WALL_MS = Number(process.env.PROBE_WALL_MS ?? 360_000);
const task = `Build a small website in Next.js with a cool Rubik's cube solver mechanism using three.js.
Scaffold the project, install dependencies, create the 3D cube component, and make sure it builds.`;

const t0 = Date.now();
const el = () => ((Date.now() - t0) / 1000).toFixed(1).padStart(6);
let lastEvent = Date.now();
let deltaChars = 0;
let firstDelta = 0;
let thinkingChars = 0;
let tools = 0;
let files = 0;

const hb = setInterval(() => {
  const idle = ((Date.now() - lastEvent) / 1000).toFixed(0);
  console.log(
    `[${el()}] ♥ idle ${idle}s · text ${deltaChars}c · think ${thinkingChars}c · ${tools} tools · ${files} files`,
  );
}, 15_000);

const { E2bSandboxProvider } = await import("../dist/sandbox/e2b.js");
const provider = new E2bSandboxProvider(process.env.E2B_API_KEY);
console.log(`[${el()}] creating E2B sandbox...`);
const sandbox = await provider.create({
  runId: "probe-" + Date.now(),
  egressAllowlist: [],
  cpuMs: 600_000,
  memoryMb: 2048,
  diskMb: 5120,
  wallMs: 600_000,
});
console.log(`[${el()}] sandbox ready`);

const backend = new AnthropicBackend();
const ac = new AbortController();
const wall = setTimeout(() => {
  console.log(`[${el()}] ⏱ WALL ${WALL_MS}ms hit — aborting`);
  ac.abort();
}, WALL_MS);

let output = "",
  usage;
try {
  ({ output, usage } = await backend.run({
    input: task,
    sandbox,
    signal: ac.signal,
    emit: (e) => {
      lastEvent = Date.now();
      if (e.type === "message.delta") {
        if (!firstDelta) {
          firstDelta = Date.now() - t0;
          console.log(`[${el()}] first text delta`);
        }
        deltaChars += e.text.length;
      } else if (e.type === "reasoning.available") {
        if (!thinkingChars) console.log(`[${el()}] first thinking delta`);
        thinkingChars += e.text.length;
      } else if (e.type === "tool.started") {
        tools++;
        console.log(`[${el()}] → ${e.tool}${e.preview ? ": " + e.preview.slice(0, 90) : ""}`);
      } else if (e.type === "tool.completed") {
        console.log(`[${el()}]   ${e.error ? "✗" : "✓"} ${e.tool} (${(e.duration_s ?? 0).toFixed(1)}s)`);
      } else if (e.type === "file.written") {
        files++;
        console.log(`[${el()}] 📝 ${e.path} (${e.bytes}b)`);
      } else if (e.type === "run.failed") {
        console.log(`[${el()}] ✗ run.failed: ${e.error}`);
      }
    },
  }));
} catch (err) {
  console.log(`[${el()}] ✗ EXCEPTION: ${err?.stack || err}`);
  clearInterval(hb);
  clearTimeout(wall);
  await sandbox.destroy().catch(() => {});
  process.exit(1);
}
clearInterval(hb);
clearTimeout(wall);

console.log(`\n=== DONE in ${el()}s · firstDelta ${(firstDelta / 1000).toFixed(1)}s · ${tools} tools · ${files} files ===`);
console.log(`tokens: in=${usage.inputTokens} out=${usage.outputTokens} cacheRead=${usage.cacheReadTokens}`);
console.log(`\n=== SUMMARY ===\n${output.slice(0, 1200)}`);
console.log(`\n=== VERIFY (tree + package.json) ===`);
const tree = await sandbox.exec(`find . -maxdepth 2 -not -path '*/node_modules/*' -not -path '*/.git/*' -not -path '*/.next/*' | sort | head -40`);
console.log(tree.stdout);
const pkg = await sandbox.exec(`cat package.json 2>/dev/null | head -40 || echo NONE`);
console.log("package.json:\n" + pkg.stdout);
await sandbox.destroy();
console.log(`[${el()}] destroyed`);
process.exit(0);
