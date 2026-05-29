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

const TURNSTILE_SECRET = process.env.TURNSTILE_SECRET_KEY?.trim();

// Per-IP daily caps for expensive endpoints, on top of the per-minute limit —
// bounds how much of the daily coding budget a single IP can consume.
const DAILY_LIMITS: Record<string, number> = { intent: 25 };

function takeDaily(key: string, perDay: number): boolean {
  const now = Date.now();
  let b = buckets.get(key);
  if (!b || b.resetAt < now) {
    b = { count: 0, resetAt: now + 86_400_000 };
    buckets.set(key, b);
  }
  if (b.count >= perDay) return false;
  b.count += 1;
  return true;
}

// Cloudflare Turnstile human-check. Returns true (allow) when Turnstile isn't
// configured, so the gate activates only once TURNSTILE_SECRET_KEY is set.
async function verifyTurnstile(token: string | null, ip: string): Promise<boolean> {
  if (!TURNSTILE_SECRET) return true;
  if (!token) return false;
  try {
    const r = await fetch("https://challenges.cloudflare.com/turnstile/v0/siteverify", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ secret: TURNSTILE_SECRET, response: token, remoteip: ip }),
    });
    const j = (await r.json()) as { success?: boolean };
    return j.success === true;
  } catch {
    return false;
  }
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
    if (request.method !== "GET" && request.method !== "HEAD") {
      const ip = clientIp(request);
      if (pathKey === "intent" && !(await verifyTurnstile(request.headers.get("x-turnstile-token"), ip))) {
        return NextResponse.json(
          { kind: "error", message: "Human verification failed — refresh the page and try again." },
          { status: 403 },
        );
      }
      const limit = RATE_LIMITS[pathKey];
      if (limit !== undefined && !takeBucket(`${ip}:${pathKey}`, limit)) {
        return NextResponse.json(
          {
            kind: "error",
            message: `Rate limit hit (${limit}/min). The sandbox throttles writes to keep it healthy for everyone.`,
          },
          { status: 429 },
        );
      }
      const daily = DAILY_LIMITS[pathKey];
      if (daily !== undefined && !takeDaily(`${ip}:${pathKey}:day`, daily)) {
        return NextResponse.json(
          {
            kind: "error",
            message: `Daily limit reached (${daily}/day). Come back tomorrow — this keeps the free sandbox sustainable.`,
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

  const passthrough = upstream.headers.get("content-type");
  // SSE endpoints (e.g. /intents/:id/events) hold the response open for
  // the lifetime of the run; awaiting upstream.text() would buffer the
  // whole stream until the daemon closed it and turn the live view into
  // a single post-run dump. Stream the body through verbatim instead so
  // each `data:` frame reaches the browser the moment the daemon flushes
  // it. Cache-Control and X-Accel-Buffering passthrough matches the
  // daemon's `sse_response_headers` so intermediate proxies (the Next.js
  // dev server, deployment edges) do not re-buffer.
  if (passthrough?.startsWith("text/event-stream") && upstream.body) {
    const headers: Record<string, string> = { "content-type": passthrough };
    const cacheControl = upstream.headers.get("cache-control");
    if (cacheControl) headers["cache-control"] = cacheControl;
    const accelBuffering = upstream.headers.get("x-accel-buffering");
    if (accelBuffering) headers["x-accel-buffering"] = accelBuffering;
    return new NextResponse(upstream.body, { status: upstream.status, headers });
  }

  const body = await upstream.text();
  const response = new NextResponse(body, { status: upstream.status });
  if (passthrough) response.headers.set("content-type", passthrough);
  return response;
}

export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const PATCH = forward;
export const DELETE = forward;
