// Pure selection + oldest-first ordering of dispatched intents from the
// daemon audit feed, extracted from index.mjs pollOnce so the load-bearing
// settlement ordering is unit-testable without fetching. Only
// intent_dispatched events are kept, sorted ascending by timestamp_ms with
// a missing timestamp treated as 0, so on-chain settlement order mirrors
// the order tasks actually fired.
export function selectDispatches(events) {
  return events
    .filter((e) => e?.kind?.type === "intent_dispatched")
    .sort((a, b) => (a.timestamp_ms ?? 0) - (b.timestamp_ms ?? 0));
}
