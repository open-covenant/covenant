// Writes the committed fallback snapshot the live route serves when a repo
// checkout has no .git (e.g. some deploy runtimes). The live surface is
// app/agent-stream/route.ts; this just keeps a current baseline in the bundle.
// The snapshot also carries the commit total and the loop's latest commit for
// app/api/stats.
//
// Re-run after notable commits:  node scripts/gen-agent-stream.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findRepoRoot, generateStream } from "../lib/agentStream.mjs";
import { readHistory } from "../lib/repoHistory.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = findRepoRoot(resolve(here, "..", ".."));
const outFile = resolve(here, "..", "public", "agent-stream.json");

const payload = generateStream({ repoRoot });

// Read the existing snapshot so we can avoid regressing it on a shallow build.
let existing = {};
try {
  existing = JSON.parse(readFileSync(outFile, "utf8"));
} catch {}

// Bake the commit total and the loop's latest commit for the stats route to fall
// back on, even on a build that keeps the existing replay lines.
const history = readHistory({ repoRoot });
if (history.commits === null) {
  console.warn(`gen-agent-stream: could not read the full history (${history.reason}); keeping the snapshot's total and head`);
}
const stats = {
  totalCommits: Math.max(history.commits || 0, existing.totalCommits || 0),
  head: history.head || existing.head || null,
};

// Never clobber the committed snapshot with a thinner one. A shallow build
// checkout (e.g. Render clones depth=1, or no git at all) yields little or no
// history; keep the existing richer replay so the fallback stays full. Don't
// fail the build either way.
const thinner = !payload.lines.length || payload.commits < (existing.commits || 0);
if (thinner) {
  console.warn(
    `gen-agent-stream: generated ${payload.commits} commits (< existing ${existing.commits || 0}); keeping existing lines`,
  );
}

const stream = thinner ? existing : payload;
const json = JSON.stringify({ ...stream, ...stats });
writeFileSync(outFile, json, "utf8");

console.log(`wrote ${outFile}`);
console.log(
  `  ${stream.lines?.length ?? 0} lines · ${stats.totalCommits} total commits · loop head ${stats.head ?? "unknown"} · ${(Buffer.byteLength(json) / 1024).toFixed(1)} KB`,
);
