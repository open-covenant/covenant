import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const REPO = "open-covenant/covenant";
const BRANCH = "main";
// The strip counts only the autonomous loop's own commits — the work the
// agent shipped, not human or merge commits. The loop authors as "Covenant".
const LOOP_AUTHOR = "covenant@users.noreply.github.com";
// The loop's first commit — the moment Covenant began building in the open.
// Immutable history, so a constant is authoritative.
const ALPHA_SINCE = "2026-05-09T20:43:52+02:00";
const TTL = 120_000;
const FETCH_TIMEOUT = 4000;
let cache: { at: number; body: string } | null = null;

const git = (root: string, args: string[]) =>
  execFileSync("git", ["-C", root, ...args], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }).trim();

function parseMetrics(md: string) {
  return {
    tests: md.match(/([\d,]+)\s+source-discovered Rust tests/i)?.[1] ?? null,
    live: md.match(/([\d,]+)\s+live boundary tests/i)?.[1] ?? null,
    crates: md.match(/(\d+)\s+Rust crates/i)?.[1] ?? null,
  };
}

function ghHeaders() {
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
  return {
    "user-agent": "covenant-hud",
    accept: "application/vnd.github+json",
    ...(token ? { authorization: `Bearer ${token}` } : {}),
  };
}

// Read head + total commit count straight from GitHub so the strip tracks
// origin/main regardless of when the site last deployed — the deployed checkout
// is frozen at deploy time and shallow, so its own git can't tell. per_page=1
// returns the latest sha; the Link header's last page is the commit total.
async function githubHead(signal: AbortSignal) {
  const res = await fetch(
    `https://api.github.com/repos/${REPO}/commits?sha=${BRANCH}&author=${encodeURIComponent(LOOP_AUTHOR)}&per_page=1`,
    { headers: ghHeaders(), signal, cache: "no-store" },
  );
  if (!res.ok) throw new Error(`gh ${res.status}`);
  const arr = (await res.json()) as Array<{ sha?: string }>;
  const last = (res.headers.get("link") ?? "").match(/[?&]page=(\d+)>;\s*rel="last"/i);
  return {
    // Latest loop commit, so the "committed" link points at the agent's own work.
    head: arr[0]?.sha?.slice(0, 7) ?? null,
    commits: last ? Number(last[1]) : arr.length ? 1 : null,
  };
}

async function githubMetrics(signal: AbortSignal) {
  const res = await fetch(`https://raw.githubusercontent.com/${REPO}/${BRANCH}/README.md`, {
    signal,
    cache: "no-store",
  });
  if (!res.ok) throw new Error(`readme ${res.status}`);
  return parseMetrics(await res.text());
}

export async function GET() {
  const now = Date.now();
  if (cache && now - cache.at < TTL) {
    return new NextResponse(cache.body, { headers: { "content-type": "application/json" } });
  }

  let head: string | null = null;
  let commits: number | null = null;
  let metrics = { tests: null as string | null, live: null as string | null, crates: null as string | null };

  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), FETCH_TIMEOUT);
  const [gh, gm] = await Promise.allSettled([githubHead(ctrl.signal), githubMetrics(ctrl.signal)]);
  clearTimeout(timer);
  if (gh.status === "fulfilled") ({ head, commits } = gh.value);
  if (gm.status === "fulfilled") metrics = gm.value;

  // Fall back to the deployed checkout, then the build-time snapshot, when
  // GitHub is unreachable. Never let the count regress below what we know.
  const root = findRepoRoot(process.cwd());
  if (root) {
    if (head === null) {
      try {
        head = git(root, ["log", `--author=${LOOP_AUTHOR}`, "-1", "--format=%h"]) || null;
      } catch {}
    }
    if (commits === null) {
      try {
        commits = Number(git(root, ["rev-list", "--count", `--author=${LOOP_AUTHOR}`, "HEAD"])) || null;
      } catch {}
    }
    if (metrics.tests === null) {
      try {
        metrics = parseMetrics(readFileSync(join(root, "README.md"), "utf8"));
      } catch {}
    }
  }
  try {
    const snap = JSON.parse(readFileSync(join(process.cwd(), "public", "agent-stream.json"), "utf8"));
    if (snap.totalCommits) commits = Math.max(commits ?? 0, snap.totalCommits);
  } catch {}

  const body = JSON.stringify({ commits, head, alphaSince: ALPHA_SINCE, ...metrics });
  cache = { at: now, body };
  return new NextResponse(body, {
    headers: { "content-type": "application/json", "cache-control": "public, max-age=120" },
  });
}
