import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot } from "@/lib/agentStream.mjs";
import { LOOP_AUTHOR } from "@/lib/repoHistory.mjs";
import { ghHeaders, parseMetrics } from "./_stats";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const REPO = "open-covenant/covenant";
const BRANCH = "main";
// The loop's first commit — the moment Covenant began building in the open.
// Immutable history, so a constant is authoritative.
const ALPHA_SINCE = "2026-05-09T20:43:52+02:00";
// Each refresh makes two GitHub API reads, and GitHub allows an unauthenticated
// IP 60 an hour. Refreshing every five minutes keeps this route to 24 of them.
const TTL = 300_000;
const FETCH_TIMEOUT = 4000;
let cache: { at: number; body: string } | null = null;
// The last values GitHub returned, reused when a later read is refused so the
// header keeps them instead of showing a dash.
let lastGood: { head: string | null; commits: number | null } | null = null;

const git = (root: string, args: string[]) =>
  execFileSync("git", ["-C", root, ...args], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }).trim();

// Read the commit total and the loop's latest commit from GitHub, so the header
// tracks main between deploys; the deployed checkout is shallow and frozen at
// deploy time. With per_page=1 the Link header's last page is the total. The
// linked sha comes only from the author-filtered read, since the tip may be a
// human or merge commit, so it is null when that read is refused.
async function githubHead(signal: AbortSignal) {
  const total = fetch(`https://api.github.com/repos/${REPO}/commits?sha=${BRANCH}&per_page=1`, {
    headers: ghHeaders(),
    signal,
    cache: "no-store",
  });
  const loop = fetch(
    `https://api.github.com/repos/${REPO}/commits?sha=${BRANCH}&author=${encodeURIComponent(LOOP_AUTHOR)}&per_page=1`,
    { headers: ghHeaders(), signal, cache: "no-store" },
  );
  const [tRes, lRes] = await Promise.all([total, loop]);
  if (!tRes.ok) throw new Error(`gh ${tRes.status}`);
  const last = (tRes.headers.get("link") ?? "").match(/[?&]page=(\d+)>;\s*rel="last"/i);
  const tArr = (await tRes.json()) as Array<{ sha?: string }>;
  let head: string | null = null;
  if (lRes.ok) {
    const lArr = (await lRes.json()) as Array<{ sha?: string }>;
    if (lArr[0]?.sha) head = lArr[0].sha.slice(0, 7);
  }
  return {
    head,
    commits: last ? Number(last[1]) : tArr.length ? 1 : null,
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
  if (gh.status === "fulfilled") {
    head = gh.value.head ?? lastGood?.head ?? null;
    commits = gh.value.commits ?? lastGood?.commits ?? null;
    lastGood = { head, commits };
  } else if (lastGood) {
    ({ head, commits } = lastGood);
  }
  if (gm.status === "fulfilled") metrics = gm.value;

  // Fill whatever GitHub and the last reading did not give from the deployed
  // checkout, then the build-time snapshot. The checkout is shallow, so it is
  // usually the snapshot that names the loop's latest commit. Never let the count
  // regress below what we know.
  const root = findRepoRoot(process.cwd());
  if (root) {
    if (head === null) {
      try {
        head = git(root, ["log", `--author=${LOOP_AUTHOR}`, "-1", "--format=%H"]).slice(0, 7) || null;
      } catch {}
    }
    if (commits === null) {
      try {
        commits = Number(git(root, ["rev-list", "--count", "HEAD"])) || null;
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
    if (head === null && typeof snap.head === "string" && snap.head) head = snap.head;
  } catch {}

  const body = JSON.stringify({ commits, head, alphaSince: ALPHA_SINCE, ...metrics });
  cache = { at: now, body };
  return new NextResponse(body, {
    headers: { "content-type": "application/json", "cache-control": `public, max-age=${TTL / 1000}` },
  });
}
