// Writes the committed fallback snapshot the live route serves when a repo
// checkout has no .git (e.g. some deploy runtimes). The live surface is
// app/agent-stream/route.ts; this just keeps a current baseline in the bundle.
//
// Re-run after notable commits:  node scripts/gen-agent-stream.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findRepoRoot, generateStream } from "../lib/agentStream.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = findRepoRoot(resolve(here, "..", ".."));
const outFile = resolve(here, "..", "public", "agent-stream.json");

const payload = generateStream({ repoRoot });

// Never clobber the committed snapshot with a thinner one. A shallow build
// checkout (e.g. Render clones depth=1, or no git at all) yields little or no
// history; keep the existing richer snapshot so the replay fallback stays full.
// Don't fail the build either way.
let existingCommits = 0;
try {
  existingCommits = JSON.parse(readFileSync(outFile, "utf8")).commits || 0;
} catch {}
if (!payload.lines.length || payload.commits < existingCommits) {
  console.warn(
    `gen-agent-stream: generated ${payload.commits} commits (< existing ${existingCommits}) — keeping existing snapshot`,
  );
  process.exit(0);
}

const json = JSON.stringify(payload);
writeFileSync(outFile, json, "utf8");

console.log(`wrote ${outFile}`);
console.log(`  ${payload.lines.length} lines · ${(Buffer.byteLength(json) / 1024).toFixed(1)} KB`);
