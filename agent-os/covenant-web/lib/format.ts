// Format helpers. Wrap raw daemon primitives (hashes, pubkeys, signatures,
// timestamps, capability scope expressions) into something readable as
// primary UI content. The original primitive lives under "raw json" expand
// or the Developer mode toggle.

import type { AgentId } from "./api";

export function shortHash(value: string, length = 10): string {
  if (!value) return "";
  if (value.length <= length) return value;
  return `${value.slice(0, length)}…`;
}

export function shortPubkey(pubkey: string): string {
  if (!pubkey) return "";
  if (pubkey.length <= 12) return pubkey;
  return `${pubkey.slice(0, 4)}…${pubkey.slice(-4)}`;
}

/**
 * Human-friendly name for an agent identity. Prefers display name; falls
 * back to a short pubkey slice. Never returns the raw 32-byte hex as
 * primary content.
 */
export function formatAgentName(id: AgentId | { display?: string; pubkey: string }): string {
  if (id.display && id.display.trim()) return id.display.trim();
  return `Agent ${shortPubkey(id.pubkey)}`;
}

/**
 * Single-segment friendly name for a peer captured in a capability scope
 * like `a2a.send.<pubkey-or-display>`. The slug can be either a base58
 * pubkey or a synthesized display from the daemon.
 */
export function formatPeerName(slug: string): string {
  if (!slug) return "an agent";
  if (slug.length >= 32) return `agent ${shortPubkey(slug)}`;
  return slug;
}

/**
 * Short, status-bearing rendering of a hex hash. We show only the leading
 * 8 chars by default; the full string belongs in a "Show signature" expand.
 * The leading label (e.g. "Result hash") frames what the hash represents.
 */
export function formatHashStatus(hex: string, label = "Hash"): string {
  if (!hex) return `${label}: —`;
  return `${label}: ${shortHash(hex, 10)}`;
}

/**
 * Render a verification outcome for an integrity check. Compact, visual,
 * no raw hex in the primary surface.
 */
export function formatVerifyStatus(valid: boolean): string {
  return valid ? "✓ verified" : "⨯ tampered";
}

export function formatTimestamp(ms: number, opts?: { withSeconds?: boolean }): string {
  const date = new Date(ms);
  return date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    ...(opts?.withSeconds ? { second: "2-digit" } : {}),
  });
}

export function formatRelative(ms: number, now: number = Date.now()): string {
  const delta = Math.max(0, Math.floor((now - ms) / 1000));
  if (delta < 5) return "just now";
  if (delta < 60) return `${delta}s ago`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  return `${Math.floor(delta / 86400)}d ago`;
}

export function formatDateTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Truncate a long string with an ellipsis. Mirror of `audit.truncate` so
 * consumers don't have to import both files just to clip text.
 */
export function truncate(value: string, length: number): string {
  if (value.length <= length) return value;
  return `${value.slice(0, length - 1)}…`;
}

/**
 * Translate a capability scope object into one short sentence. Scope shapes
 * are heterogeneous coming off the daemon; we render the most common
 * forms inline and fall back to "specific scope" for anything we don't
 * recognise — the raw scope object stays available under "raw json".
 */
export function formatScope(scope: unknown): string {
  if (scope === null || scope === undefined) return "any context";
  if (typeof scope === "string") {
    if (scope === "global" || scope === "any") return "any context";
    return scope;
  }
  if (typeof scope === "object") {
    const obj = scope as Record<string, unknown>;
    if (Object.keys(obj).length === 0) return "any context";
    if (obj.kind === "global") return "any context";
    if (typeof obj.agent_id === "string") return `agent ${obj.agent_id}`;
    if (typeof obj.path === "string") return `path ${obj.path}`;
  }
  return "specific scope";
}
