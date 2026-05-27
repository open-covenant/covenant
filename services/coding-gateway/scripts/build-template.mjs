// Builds the `covenant-coder` E2B template: the e2b base (node + python + tools)
// with more memory and a warm npm cache, so heavy scaffolds (Next.js + three.js)
// install fast and reliably instead of OOMing on the 1024MB default.
//
//   node --env-file=../../.env scripts/build-template.mjs   (run from services/coding-gateway)
import { Template } from "e2b";

const NAME = process.env.E2B_TEMPLATE_NAME || "covenant-coder";

const t = Template()
  .fromBaseImage()
  // Warm ~/.npm with the deps heavy scaffolds pull, then drop the throwaway
  // project — the cache survives so the agent's installs hit it.
  .runCmd(
    "mkdir -p /tmp/warm && cd /tmp/warm && npm init -y >/dev/null 2>&1 && " +
      "npm install --no-audit --no-fund next@latest react react-dom three @types/three typescript vite @vitejs/plugin-react >/dev/null 2>&1 || true; " +
      "cd / && rm -rf /tmp/warm",
  );

console.log(`building E2B template '${NAME}' (cpu 4, memory 4096)…`);
try {
  const info = await Template.build(t, NAME, {
    cpuCount: 4,
    memoryMB: 4096,
    onBuildLogs: (l) => process.stdout.write(`  ${typeof l === "string" ? l : (l?.message ?? JSON.stringify(l))}\n`),
  });
  console.log("\nBUILT:", JSON.stringify(info));
} catch (e) {
  console.log("\nBUILD FAILED:", e?.message || e);
  process.exit(1);
}
