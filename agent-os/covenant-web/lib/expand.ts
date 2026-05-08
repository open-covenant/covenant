// Mirror of the CLI's pubkey-prefix expansion logic for `capabilities grant`.
//
// When the operator grants `a2a.{send,recv,respond}.<X>` and `<X>` looks like a
// pubkey-prefix (no `@`, no further `.`, non-empty), the unique live peer
// matching the prefix supplies the full b58 pubkey that the daemon's capability
// store keys on. Subjects-on-pubkey is the durable invariant; the display form
// is collision-prone (Sprint 59), so the rewrite happens client-side and the
// stored capability is a stable handle on the peer's identity.
//
// Reference: `crates/covenant/src/main.rs` — `peer_prefix_to_lookup`,
// `expand_a2a_action`, `PEER_LOOKUP_LIMIT`, `PEER_SCOPED_PREFIXES`. Behavioural
// parity with the CLI is the point — operators alternate between web and CLI
// against the same daemon and the predicate must accept the same set of
// strings on both surfaces.

import type { PeerSummary } from "@/lib/api";

export const PEER_LOOKUP_LIMIT = 16;

export const PEER_SCOPED_PREFIXES = [
  "a2a.send.",
  "a2a.recv.",
  "a2a.respond.",
] as const;

export function peerPrefixToLookup(action: string): string | null {
  for (const prefix of PEER_SCOPED_PREFIXES) {
    if (action.startsWith(prefix)) {
      const tail = action.slice(prefix.length);
      if (tail.length === 0 || tail.includes(".") || tail.includes("@")) {
        return null;
      }
      return tail;
    }
  }
  return null;
}

export type ExpandOutcome =
  | { kind: "unchanged" }
  | { kind: "rewritten"; full: string; peerPubkeyB58: string };

export type ExpandError =
  | { kind: "no_match"; tail: string }
  | { kind: "ambiguous"; tail: string; matches: PeerSummary[] }
  | { kind: "revoked_only"; tail: string; matches: PeerSummary[] };

export type ExpandResult =
  | { ok: true; value: ExpandOutcome }
  | { ok: false; error: ExpandError };

export function expandA2aAction(
  action: string,
  peers: PeerSummary[],
): ExpandResult {
  let prefix: string | null = null;
  let tail: string | null = null;
  for (const p of PEER_SCOPED_PREFIXES) {
    if (action.startsWith(p)) {
      prefix = p.slice(0, -1);
      tail = action.slice(p.length);
      break;
    }
  }
  if (prefix === null || tail === null) {
    return { ok: true, value: { kind: "unchanged" } };
  }
  if (tail.length === 0 || tail.includes(".") || tail.includes("@")) {
    return { ok: true, value: { kind: "unchanged" } };
  }

  const live: PeerSummary[] = [];
  const revoked: PeerSummary[] = [];
  for (const peer of peers) {
    if (!peer.agent_id.pubkey.startsWith(tail)) continue;
    if (peer.revoked_at !== null) {
      revoked.push(peer);
    } else {
      live.push(peer);
    }
  }

  if (live.length === 1) {
    const pubkey = live[0].agent_id.pubkey;
    return {
      ok: true,
      value: {
        kind: "rewritten",
        full: `${prefix}.${pubkey}`,
        peerPubkeyB58: pubkey,
      },
    };
  }
  if (live.length === 0 && revoked.length === 0) {
    return { ok: false, error: { kind: "no_match", tail } };
  }
  if (live.length === 0) {
    return { ok: false, error: { kind: "revoked_only", tail, matches: revoked } };
  }
  return { ok: false, error: { kind: "ambiguous", tail, matches: live } };
}

export function formatExpandError(err: ExpandError): string {
  switch (err.kind) {
    case "no_match":
      return (
        `no peer matched pubkey-prefix \`${err.tail}\` — ` +
        "paste from the registered-peers list above"
      );
    case "ambiguous": {
      const list = err.matches
        .map(
          (p) =>
            `${p.agent_id.display} (pubkey ${p.agent_id.pubkey.slice(0, 8)}…)`,
        )
        .join(", ");
      return `pubkey-prefix \`${err.tail}\` matched ${err.matches.length} live peers — narrow it: ${list}`;
    }
    case "revoked_only": {
      const list = err.matches
        .map(
          (p) =>
            `${p.agent_id.display} (pubkey ${p.agent_id.pubkey.slice(0, 8)}…)`,
        )
        .join(", ");
      return `pubkey-prefix \`${err.tail}\` matched only revoked peer(s): ${list}`;
    }
  }
}
