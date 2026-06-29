// Duration formatter for the header's "up" cell, kept out of the client
// component so it can be unit-tested. `now` is injectable (defaulting to the
// wall clock) so elapsed time is deterministic under test.

export function uptime(sinceISO: string, now = Date.now()): string {
  const ms = now - new Date(sinceISO).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "—";
  const d = Math.floor(ms / 86_400_000);
  const h = Math.floor(ms / 3_600_000) % 24;
  const m = Math.floor(ms / 60_000) % 60;
  return `${d}d ${h}h ${m}m`;
}
