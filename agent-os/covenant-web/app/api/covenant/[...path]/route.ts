import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { NextRequest, NextResponse } from "next/server";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

const DAEMON_URL = process.env.COVENANT_DAEMON_URL ?? "http://127.0.0.1:8421";
const TOKEN_PATH =
  process.env.COVENANT_OPERATOR_TOKEN_PATH ??
  join(process.env.COVENANT_HOME ?? join(homedir(), ".covenant"), "peers", "operator.token");

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

// In demo mode, destructive endpoints are blocked at the proxy layer.
// The daemon is on a private Render network so this is the only ingress.
const DESTRUCTIVE_PATHS = new Set([
  "peers/rotate",
  "peers/purge",
  "audit/purge",
  "capabilities/purge",
  "capabilities/revoke",
  "memory/purge",
  "memory/repair",
  "memory/compact",
  "a2a/compact",
  "a2a/repair",
]);

// Rate limits per minute per IP for write endpoints.
const RATE_LIMITS: Record<string, number> = {
  intent: 10,
  "capabilities/grant": 5,
  "a2a/tasks": 10,
  "a2a/results": 10,
  "tools/call": 10,
};

type Bucket = { count: number; resetAt: number };
const buckets = new Map<string, Bucket>();
let lastCleanup = 0;

function sweep(now: number) {
  if (now - lastCleanup < 30_000) return;
  lastCleanup = now;
  for (const [k, b] of buckets) {
    if (b.resetAt < now) buckets.delete(k);
  }
}

function takeBucket(key: string, perMinute: number): boolean {
  const now = Date.now();
  sweep(now);
  let bucket = buckets.get(key);
  if (!bucket || bucket.resetAt < now) {
    bucket = { count: 0, resetAt: now + 60_000 };
    buckets.set(key, bucket);
  }
  if (bucket.count >= perMinute) return false;
  bucket.count += 1;
  return true;
}

function clientIp(request: NextRequest): string {
  const xff = request.headers.get("x-forwarded-for");
  if (xff) return xff.split(",")[0]!.trim();
  return request.headers.get("x-real-ip") ?? "anon";
}

async function readOperatorToken(): Promise<string | null> {
  const literal = process.env.COVENANT_OPERATOR_TOKEN?.trim();
  if (literal) return literal;
  try {
    const raw = await readFile(TOKEN_PATH, "utf8");
    return raw.trim() || null;
  } catch {
    return null;
  }
}

async function forward(
  request: NextRequest,
  ctx: { params: Promise<{ path: string[] }> },
): Promise<NextResponse> {
  const { path } = await ctx.params;
  if (!path?.length || path.some((segment) => segment === "..")) {
    return NextResponse.json({ kind: "error", message: "invalid path" }, { status: 400 });
  }

  const pathKey = path.join("/");

  if (DEMO_MODE) {
    if (DESTRUCTIVE_PATHS.has(pathKey)) {
      return NextResponse.json(
        {
          kind: "error",
          message: "This action is disabled in the public sandbox.",
        },
        { status: 403 },
      );
    }
    const limit = RATE_LIMITS[pathKey];
    if (limit !== undefined && request.method !== "GET" && request.method !== "HEAD") {
      const ip = clientIp(request);
      if (!takeBucket(`${ip}:${pathKey}`, limit)) {
        return NextResponse.json(
          {
            kind: "error",
            message: `Rate limit hit (${limit}/min). The sandbox throttles writes to keep it healthy for everyone.`,
          },
          { status: 429 },
        );
      }
    }
  }

  const token = await readOperatorToken();
  if (!token) {
    return NextResponse.json(
      {
        kind: "error",
        message:
          "operator token unavailable: set COVENANT_OPERATOR_TOKEN or ensure the daemon has bootstrapped operator.token",
      },
      { status: 503 },
    );
  }

  const targetUrl = new URL(`${DAEMON_URL}/${pathKey}`);
  request.nextUrl.searchParams.forEach((value, key) => targetUrl.searchParams.append(key, value));

  const headers: Record<string, string> = {
    Authorization: `Bearer ${token}`,
  };
  const contentType = request.headers.get("content-type");
  if (contentType) headers["content-type"] = contentType;

  const init: RequestInit = { method: request.method, headers };
  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = await request.text();
  }

  let upstream: Response;
  try {
    upstream = await fetch(targetUrl.toString(), init);
  } catch (err) {
    return NextResponse.json(
      {
        kind: "error",
        message: `daemon unreachable at ${DAEMON_URL}: ${err instanceof Error ? err.message : String(err)}`,
      },
      { status: 502 },
    );
  }

  const body = await upstream.text();
  const response = new NextResponse(body, { status: upstream.status });
  const passthrough = upstream.headers.get("content-type");
  if (passthrough) response.headers.set("content-type", passthrough);
  return response;
}

export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const PATCH = forward;
export const DELETE = forward;
