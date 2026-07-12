// Pure replay of sigs.jsonl into the in-memory sig table, extracted from
// index.mjs so the restart-replay resilience is unit-testable without
// booting the sidecar. Returns a Map keyed by intent_id containing only
// rows that have both intent_id and tx_sig; blank and malformed lines
// (including JSON null/non-objects) are skipped so a partial or corrupt
// trailing write never crashes boot.
export function restoreSigs(text) {
  const byIntent = new Map();
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    try {
      const row = JSON.parse(line);
      if (row.intent_id && row.tx_sig) {
        byIntent.set(row.intent_id, row);
      }
    } catch {
      continue;
    }
  }
  return byIntent;
}
