// Writes the committed fallback snapshot the live route serves when a repo
// checkout has no .git (e.g. some deploy runtimes). The live surface is
// app/agent-stream/route.ts; this just keeps a current baseline in the bundle.
//
// Re-run after notable commits:  node scripts/gen-agent-stream.mjs

import { writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findRepoRoot, generateStream } from "../lib/agentStream.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = findRepoRoot(resolve(here, "..", ".."));
const outFile = resolve(here, "..", "public", "agent-stream.json");

const payload = generateStream({ repoRoot });

// If git isn't available at build time (no .git / no git binary), generateStream
// yields nothing. Keep the existing committed snapshot rather than clobbering it
// with an empty one — the replay fallback must never go blank. Don't fail the build.
if (!payload.lines.length) {
  console.warn("gen-agent-stream: no git history available — keeping existing snapshot");
  process.exit(0);
}

const json = JSON.stringify(payload);
writeFileSync(outFile, json, "utf8");

console.log(`wrote ${outFile}`);
console.log(`  ${payload.lines.length} lines · ${(Buffer.byteLength(json) / 1024).toFixed(1)} KB`);
