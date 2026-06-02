import { bus } from "@/lib/agentBus.mjs";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// SSE feed of the live covenant loop. Replays the recent ring on connect,
// then streams each new transition/commit as it happens.
export function GET() {
  bus.startLocalTail();

  const enc = new TextEncoder();
  let unsub = () => {};
  let hb: ReturnType<typeof setInterval>;

  const stream = new ReadableStream({
    start(controller) {
      const send = (e: unknown) => {
        try {
          controller.enqueue(enc.encode(`data: ${JSON.stringify(e)}\n\n`));
        } catch {}
      };
      controller.enqueue(enc.encode("retry: 3000\n\n"));
      for (const e of bus.ring) send(e);
      unsub = bus.subscribe(send);
      hb = setInterval(() => {
        try {
          controller.enqueue(enc.encode(": hb\n\n"));
        } catch {}
      }, 15000);
    },
    cancel() {
      unsub();
      clearInterval(hb);
    },
  });

  return new Response(stream, {
    headers: {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-cache, no-transform",
      connection: "keep-alive",
      "x-accel-buffering": "no",
    },
  });
}
