import { NextResponse } from "next/server";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

// Server-side proxy to the settlement-publisher sidecar. The sidecar
// is a private Render service (pserv) reachable only over the private
// network, so the browser cannot fetch it directly. This route fans
// the public /sigs endpoint through to clients.
//
// Response shape mirrors the sidecar exactly:
//   { cluster: "devnet" | "mainnet" | ..., sigs: { [intent_id]: { tx_sig, slot, settled_at_ms, intent_text, cluster } } }

const PUBLISHER_URL =
  process.env.SETTLEMENT_PUBLISHER_URL ??
  "http://covenant-settlement-publisher:10001";

export async function GET() {
  try {
    const res = await fetch(`${PUBLISHER_URL}/sigs`, {
      cache: "no-store",
      // Short timeout: the sidecar is a sibling service; if it's down
      // we'd rather degrade gracefully than block page loads.
      signal: AbortSignal.timeout(2500),
    });
    if (!res.ok) {
      return NextResponse.json(
        { cluster: "devnet", sigs: {}, error: `publisher ${res.status}` },
        { status: 200 },
      );
    }
    const body = await res.json();
    return NextResponse.json(body, {
      status: 200,
      headers: { "Cache-Control": "no-store, max-age=0" },
    });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    return NextResponse.json(
      { cluster: "devnet", sigs: {}, error: msg },
      { status: 200 },
    );
  }
}
