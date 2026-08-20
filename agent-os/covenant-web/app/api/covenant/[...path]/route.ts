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

// Rate limits per minute per IP for write endpoints. `intents/resume`
// re-executes a past intent and spends the coding budget just like
// `intent`, so it carries the same per-minute ceiling.
const RATE_LIMITS: Record<string, number> = {
  intent: 10,
  "intents/resume": 10,
  "capabilities/grant": 5,
  "a2a/tasks": 10,
  "a2a/results": 10,
  "tools/call": 10,
};

// Per-IP daily cap on budget-spending endpoints. `intent` and
// `intents/resume` both consume the same daily coding budget, so they
// draw from one shared bucket — otherwise an attacker doubles the cap by
// alternating the two.
const DAILY_INTENTS = 25;

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

// The client IP used for rate limiting. Cloudflare sets `CF-Connecting-IP`
// to the real client address and overwrites any value the client supplies,
// so it cannot be spoofed — unlike `X-Forwarded-For`, whose leftmost entry
// is fully attacker-controlled (CF appends, never strips). Reading XFF's
// leftmost hop let anyone rotate the rate-limit bucket and drain the daily
// coding budget. Prefer the trusted CF headers; fall back to the *rightmost*
// XFF hop (the one closest to our proxy) only when CF isn't in front.
function clientIp(request: NextRequest): string {
  const cf = request.headers.get("cf-connecting-ip")?.trim();
  if (cf) return cf;
  const trueClient = request.headers.get("true-client-ip")?.trim();
  if (trueClient) return trueClient;
  const xff = request.headers.get("x-forwarded-for");
  if (xff) {
    const hops = xff.split(",").map((h) => h.trim()).filter(Boolean);
    if (hops.length) return hops[hops.length - 1]!;
  }
  return request.headers.get("x-real-ip")?.trim() ?? "anon";
}

const TURNSTILE_SECRET = process.env.TURNSTILE_SECRET_KEY?.trim();

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

// Capability namespaces that grant operator/system-level authority or
// arbitrary tool invocation. The daemon's whole authorization model is
// capability-based, and the public proxy forwards every request under the
// operator token — so an anonymous caller self-granting one of these would
// hold the same access as the operator. `tool.call.*` is singled out because
// it authorizes invoking arbitrary tools (fetch/http_request → SSRF). These
// are never grantable from the public sandbox and never shown in the public
// capability registry.
const PRIVILEGED_NS = new Set([
  "admin",
  "system",
  "operator",
  "operators",
  "peer",
  "peers",
  "root",
  "capability",
  "capabilities",
  "audit",
  "budget",
  "spend",
  "settlement",
]);

function isPrivilegedAction(action: unknown): boolean {
  if (typeof action !== "string" || !action) return true;
  if (action.includes("*")) return true;
  if (action === "tool.call" || action.startsWith("tool.call.") || action.startsWith("tool.call:")) {
    return true;
  }
  const ns = action.split(/[.:]/)[0]!.toLowerCase();
  return PRIVILEGED_NS.has(ns);
}

// Tool names that reach the network or filesystem. Even though these tools
// aren't registered in the sandbox runtime today, blocking them at the proxy
// keeps the SSRF surface closed if they ever are.
const TOOL_DENY = new Set([
  "fetch",
  "http",
  "https",
  "http_request",
  "httprequest",
  "request",
  "curl",
  "wget",
  "url",
  "open_url",
  "web_fetch",
  "browse",
  "exec",
  "shell",
  "bash",
  "sh",
  "run",
  "read_file",
  "write_file",
  "readfile",
  "writefile",
  "fs",
  "file",
]);

// GET responses whose bodies leak other visitors' content or daemon
// internals. The public sandbox keeps these views live (they showcase the
// audit/memory/capability primitives) but strips the sensitive fields.
const REDACT_GET = new Set(["memory/recent", "memory/search", "audit/recent", "capabilities/recent", "verify"]);

const UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;

function scrubIds(s: unknown): unknown {
  return typeof s === "string" ? s.replace(UUID_RE, "[id]") : s;
}

function redactResponse(pathKey: string, json: unknown): unknown {
  if (!json || typeof json !== "object") return json;
  const obj = json as Record<string, unknown>;

  if (pathKey === "memory/recent" || pathKey === "memory/search") {
    // Strip the record text, embedding vector, owner, and metadata — these
    // carry users' AI-generated source code and identifiers. Keep only the
    // structural fields the showcase needs.
    if (Array.isArray(obj.records)) {
      obj.records = obj.records.map((r) => {
        const rec = (r ?? {}) as Record<string, unknown>;
        return {
          id: rec.id,
          tier: rec.tier,
          created_at: rec.created_at,
          text: "[redacted in public sandbox]",
        };
      });
    }
    return obj;
  }

  if (pathKey === "capabilities/recent") {
    if (Array.isArray(obj.capabilities)) {
      obj.capabilities = obj.capabilities.filter((c) => {
        const action = (c as { capability?: { action?: unknown } } | null)?.capability?.action;
        return !isPrivilegedAction(action);
      });
    }
    return obj;
  }

  if (pathKey === "verify") {
    // Drop the internal record UUIDs from drift rows and scrub any UUIDs
    // embedded in the human-readable strings; keep counts and kinds.
    if (Array.isArray(obj.drift)) {
      obj.drift = obj.drift.map((d) => {
        const row = (d ?? {}) as Record<string, unknown>;
        return { kind: row.kind, message: scrubIds(row.message), repair: scrubIds(row.repair) };
      });
    }
    return obj;
  }

  if (pathKey === "audit/recent") {
    if (Array.isArray(obj.events)) {
      for (const e of obj.events) {
        const kind = (e as { kind?: unknown } | null)?.kind;
        if (kind && typeof kind === "object") {
          const k = kind as Record<string, unknown>;
          if ("intent_text" in k) k.intent_text = "[redacted]";
          if ("text" in k) k.text = "[redacted]";
        }
      }
    }
    return obj;
  }

  return obj;
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

function denied(message: string, status = 403): NextResponse {
  return NextResponse.json({ kind: "error", message }, { status });
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
  const isWrite = request.method !== "GET" && request.method !== "HEAD";
  const rawBody = isWrite ? await request.text() : undefined;

  if (DEMO_MODE) {
    if (DESTRUCTIVE_PATHS.has(pathKey)) {
      return denied("This action is disabled in the public sandbox.");
    }
    if (isWrite) {
      const ip = clientIp(request);
      const spendsBudget = pathKey === "intent" || pathKey === "intents/resume";

      if (spendsBudget && !(await verifyTurnstile(request.headers.get("x-turnstile-token"), ip))) {
        return denied("Human verification failed — refresh the page and try again.");
      }

      if (pathKey === "capabilities/grant") {
        let action: unknown;
        try {
          action = JSON.parse(rawBody || "{}").action;
        } catch {
          return denied("invalid request body");
        }
        if (isPrivilegedAction(action)) {
          return denied(
            "The public sandbox only grants non-privileged demo capabilities. Operator, system, and tool-invocation capabilities require operator access.",
          );
        }
      }

      if (pathKey === "tools/call") {
        let name: unknown;
        try {
          name = JSON.parse(rawBody || "{}").name;
        } catch {
          return denied("invalid request body");
        }
        if (typeof name === "string" && TOOL_DENY.has(name.toLowerCase())) {
          return denied("This tool is disabled in the public sandbox.");
        }
      }

      const limit = RATE_LIMITS[pathKey];
      if (limit !== undefined && !takeBucket(`${ip}:${pathKey}`, limit)) {
        return denied(
          `Rate limit hit (${limit}/min). The sandbox throttles writes to keep it healthy for everyone.`,
          429,
        );
      }

      if (spendsBudget && !takeDaily(`${ip}:intent:day`, DAILY_INTENTS)) {
        return denied(
          `Daily limit reached (${DAILY_INTENTS}/day). Come back tomorrow — this keeps the free sandbox sustainable.`,
          429,
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
  if (isWrite) init.body = rawBody;

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

  if (request.method === "GET" && REDACT_GET.has(pathKey) && passthrough?.includes("application/json")) {
    try {
      const redacted = redactResponse(pathKey, JSON.parse(body));
      const response = NextResponse.json(redacted, { status: upstream.status });
      return response;
    } catch {
      // Unparseable body (daemon error page, truncated stream) — fall
      // through and pass the original bytes rather than masking the error.
    }
  }

  const response = new NextResponse(body, { status: upstream.status });
  if (passthrough) response.headers.set("content-type", passthrough);
  return response;
}

export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const PATCH = forward;
export const DELETE = forward;
