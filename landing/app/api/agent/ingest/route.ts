import { bus } from "@/lib/agentBus.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Receives live events from a local pusher (the loop host) and fans them out
// to SSE subscribers. Disabled unless AGENT_INGEST_TOKEN is set; bearer-gated.
export async function POST(req: Request) {
  const token = process.env.AGENT_INGEST_TOKEN;
  if (!token) return new Response("ingest disabled", { status: 503 });
  if (req.headers.get("authorization") !== `Bearer ${token}`) {
    return new Response("unauthorized", { status: 401 });
  }

  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return new Response("bad json", { status: 400 });
  }

  const events = Array.isArray(body) ? body : [body];
  for (const e of events) {
    if (e && typeof e === "object" && "type" in e) bus.publish(e as never);
  }
  return new Response(null, { status: 204 });
}
