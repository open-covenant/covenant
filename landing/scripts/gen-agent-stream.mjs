// Writes the committed fallback snapshot the live route serves when a repo
// checkout has no .git (e.g. some deploy runtimes). The live surface is
// app/agent-stream/route.ts; this just keeps a current baseline in the bundle.
//
// Re-run after notable commits:  node scripts/gen-agent-stream.mjs

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findRepoRoot, generateStream } from "../lib/agentStream.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = findRepoRoot(resolve(here, "..", ".."));
const outFile = resolve(here, "..", "public", "agent-stream.json");

const payload = generateStream({ repoRoot });

// Read the existing snapshot so we can avoid regressing it on a shallow build.
let existing = {};
try {
  existing = JSON.parse(readFileSync(outFile, "utf8"));
} catch {}

// Never clobber the committed snapshot with a thinner one. A shallow build
// checkout (e.g. Render clones depth=1, or no git at all) yields little or no
// history; keep the existing richer snapshot so the replay fallback stays full.
// Don't fail the build either way.
if (!payload.lines.length || payload.commits < (existing.commits || 0)) {
  console.warn(
    `gen-agent-stream: generated ${payload.commits} commits (< existing ${existing.commits || 0}) — keeping existing snapshot`,
  );
  process.exit(0);
}

// Bake the autonomous loop's commit count so the stats route can show it even
// when the runtime checkout is shallow (Render runtime git is depth=1). Counts
// only commits the loop authored ("Covenant"), matching the route's filter.
// Needs full history — present here because the build unshallows.
let totalCommits = 0;
try {
  totalCommits = Number(
    execFileSync(
      "git",
      ["-C", repoRoot, "rev-list", "--count", "--author=covenant@users.noreply.github.com", "HEAD"],
      { encoding: "utf8" },
    ).trim(),
  );
} catch {}
totalCommits = Math.max(totalCommits || 0, existing.totalCommits || 0);

const json = JSON.stringify({ ...payload, totalCommits });
writeFileSync(outFile, json, "utf8");

console.log(`wrote ${outFile}`);
console.log(
  `  ${payload.lines.length} lines · ${totalCommits} total commits · ${(Buffer.byteLength(json) / 1024).toFixed(1)} KB`,
);
