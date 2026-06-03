import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NextResponse } from "next/server";
import { findRepoRoot, generateStream } from "@/lib/agentStream.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const TTL = 60_000;
let cache: { at: number; body: string } | null = null;

// Live when the runtime checkout has .git (regenerates from real history);
// otherwise serves the committed snapshot so the terminal never goes blank.
export function GET() {
  const now = Date.now();
  if (cache && now - cache.at < TTL) {
    return new NextResponse(cache.body, {
      headers: { "content-type": "application/json" },
    });
  }

  let body: string;
  try {
    const root = findRepoRoot(process.cwd());
    const data = root ? generateStream({ repoRoot: root }) : { lines: [], commits: 0 };
    // A shallow runtime checkout (e.g. Render clones depth=1) yields only a
    // commit or two — far thinner than the committed snapshot. Prefer the
    // snapshot unless the live history is genuinely rich.
    if (!data.lines.length || (data.commits ?? 0) < 12) throw new Error("thin git history");
    body = JSON.stringify(data);
  } catch {
    body = readFileSync(join(process.cwd(), "public", "agent-stream.json"), "utf8");
  }

  cache = { at: now, body };
  return new NextResponse(body, {
    headers: {
      "content-type": "application/json",
      "cache-control": "public, max-age=60",
    },
  });
}
