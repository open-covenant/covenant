import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The autonomous loop's git identity. The "UP" counter runs from this author's
// very first commit — the moment the loop began building Covenant in the open.
const LOOP_AUTHOR = "Open Covenant Automation";
// Fallback if git is unavailable: the loop's first commit — see loopStartISO.
const ALPHA_SINCE = "2026-05-09T20:43:52+02:00";
const TTL = 60_000;
let cache: { at: number; body: string } | null = null;

const git = (root: string, args: string[]) =>
  execFileSync("git", ["-C", root, ...args], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }).trim();

// Time the "UP" counter from the autonomous loop's first commit. Immutable, so
// any failure just falls back to the const.
function loopStartISO(root: string): string {
  try {
    return (
      git(root, ["log", "--reverse", `--author=${LOOP_AUTHOR}`, "--format=%cI", "HEAD"]).split("\n")[0] ||
      ALPHA_SINCE
    );
  } catch {
    return ALPHA_SINCE;
  }
}

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
  let alphaSince = ALPHA_SINCE;
  let metrics = { tests: null as string | null, live: null as string | null, crates: null as string | null };

  if (root) {
    try {
      commits = Number(git(root, ["rev-list", "--count", "HEAD"])) || null;
    } catch {}
    try {
      head = git(root, ["rev-parse", "--short", "HEAD"]) || null;
    } catch {}
    alphaSince = loopStartISO(root);
    metrics = readmeMetrics(root);
  }

  const body = JSON.stringify({ commits, head, alphaSince, ...metrics });
  cache = { at: now, body };
  return new NextResponse(body, {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=60" },
  });
}
