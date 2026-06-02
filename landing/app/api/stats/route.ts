import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Public alpha go-live — drives the "UP" counter. Real, published date.
const ALPHA_SINCE = "2026-05-28T00:00:00Z";
const TTL = 60_000;
let cache: { at: number; body: string } | null = null;

const git = (root: string, args: string[]) =>
  execFileSync("git", ["-C", root, ...args], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }).trim();

// Source the headline metrics from the repo's own README block rather than
// inventing them — keeps the strip honest and self-updating.
function readmeMetrics(root: string) {
  try {
    const md = readFileSync(join(root, "README.md"), "utf8");
    return {
      tests: md.match(/([\d,]+)\s+source-discovered Rust tests/i)?.[1] ?? null,
      live: md.match(/([\d,]+)\s+live boundary tests/i)?.[1] ?? null,
      crates: md.match(/(\d+)\s+Rust crates/i)?.[1] ?? null,
    };
  } catch {
    return { tests: null, live: null, crates: null };
  }
}

export function GET() {
  const now = Date.now();
  if (cache && now - cache.at < TTL) {
    return new NextResponse(cache.body, { headers: { "content-type": "application/json" } });
  }

  const root = findRepoRoot(process.cwd());
  let commits: number | null = null;
  let head: string | null = null;
  let metrics = { tests: null as string | null, live: null as string | null, crates: null as string | null };

  if (root) {
    try {
      commits = Number(git(root, ["rev-list", "--count", "HEAD"])) || null;
    } catch {}
    try {
      head = git(root, ["rev-parse", "--short", "HEAD"]) || null;
    } catch {}
    metrics = readmeMetrics(root);
  }

  const body = JSON.stringify({ commits, head, alphaSince: ALPHA_SINCE, ...metrics });
  cache = { at: now, body };
  return new NextResponse(body, {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=60" },
  });
}
