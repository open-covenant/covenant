"use client";

import { useEffect, useState } from "react";
import { api, type AgentEvent } from "@/lib/api";

export type LiveAgentEvent = AgentEvent & {
  // Stable React key + chronological sort key. Assigned client-side at
  // receive time because the wire form is intentionally envelope-free —
  // the intent_id is the URL path, and per-event ids would need a
  // server-side counter to be unique across reconnects.
  receivedMs: number;
  seq: number;
};

export type IntentEventStream = {
  events: LiveAgentEvent[];
  // Open once a frame has arrived OR the connection is established;
  // null while EventSource is still in CONNECTING. Used by the UI to
  // distinguish "live, no events yet" from "stream not running".
  open: boolean;
  error: string | null;
};

// Subscribes to the daemon's SSE stream for one intent and accumulates
// every AgentEvent frame into an in-memory list. The connection closes
// on unmount or when the intent id changes; EventSource handles
// auto-reconnect on transient network failures. Events missed during a
// disconnect are not replayed — the audit chain remains the durable
// record, fetched once on page mount for historical context.
export function useIntentEventStream(intentId: string): IntentEventStream {
  const [events, setEvents] = useState<LiveAgentEvent[]>([]);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!intentId) return;
    // Reset on intent change: the stream is per-intent so a navigation
    // between two task pages must not blend events.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setEvents([]);
    setOpen(false);
    setError(null);

    const source = new EventSource(api.intentEventsUrl(intentId));
    // `cancelled` guards every setState behind the EventSource: an
    // in-flight onmessage queued before cleanup ran would otherwise
    // write into the next intent's state (and collide on the live:0
    // React key with the new stream's first frame).
    let cancelled = false;
    let seq = 0;

    source.onopen = () => {
      if (cancelled) return;
      setOpen(true);
      setError(null);
    };

    source.onmessage = (frame: MessageEvent<string>) => {
      if (cancelled) return;
      try {
        const parsed = JSON.parse(frame.data) as AgentEvent;
        const live: LiveAgentEvent = {
          ...parsed,
          receivedMs: Date.now(),
          seq: seq++,
        };
        setEvents((prev) => [...prev, live]);
      } catch (e) {
        // A malformed frame should not silently close the stream — the
        // EventSource keeps reading. Surface the parse error so an
        // upstream schema drift is visible in the UI.
        setError(e instanceof Error ? e.message : String(e));
      }
    };

    source.onerror = () => {
      if (cancelled) return;
      // EventSource auto-reconnects on transient errors; the readyState
      // tells us whether this is a recoverable blip or a hard close.
      if (source.readyState === EventSource.CLOSED) {
        setOpen(false);
        setError("event stream closed");
      } else {
        setOpen(false);
      }
    };

    return () => {
      cancelled = true;
      source.close();
    };
  }, [intentId]);

  return { events, open, error };
}
