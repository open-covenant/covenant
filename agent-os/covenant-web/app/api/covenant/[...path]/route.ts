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

  const targetUrl = new URL(`${DAEMON_URL}/${path.join("/")}`);
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
