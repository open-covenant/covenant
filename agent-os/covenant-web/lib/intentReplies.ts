// Per-tab cache for intent reply bodies.
//
// The signed audit log stores only result_hash_hex, not the reply body, so
// the body is only available at the moment of submission. We cache it in
// sessionStorage so the task detail page can render the reply text the
// operator just saw. Opening a task in a fresh tab (or after the daemon
// reset the sandbox) won't have the body — only the signed hash survives.

const PREFIX = "covenant.intent.reply.v1:";
const MAX_BYTES = 64 * 1024;

export function saveReply(intentId: string, text: string): void {
  if (typeof window === "undefined") return;
  try {
    const value = text.length > MAX_BYTES ? text.slice(0, MAX_BYTES) : text;
    window.sessionStorage.setItem(PREFIX + intentId, value);
  } catch {
    // quota exceeded or storage disabled; the audit-log hash still proves
    // the reply existed, so a missing cache is not an error condition.
  }
}

export function loadReply(intentId: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.sessionStorage.getItem(PREFIX + intentId);
  } catch {
    return null;
  }
}
