import type { AuditEvent, AuditKind } from "./api";

export type Tone = "ok" | "warn" | "danger" | "neutral";

export function auditTone(event: AuditEvent): Tone {
  switch (event.kind.type) {
    case "capability_check":
      return event.kind.passed ? "ok" : "warn";
    case "capability_granted":
    case "operator_token_rotated":
    case "peer_revoked":
      return "ok";
    case "budget_exhausted":
    case "budget_unseeded":
    case "a2a_result_rejected":
    case "authentication_failed":
    case "a2a_sender_mismatch":
    case "a2a_recipient_rejected":
    case "capability_revoke_rejected":
    case "operator_token_rotation_rejected":
    case "operator_peers_list_rejected":
    case "operator_peer_revoke_rejected":
      return "danger";
    case "intent_ignored":
      return "warn";
    default:
      return "neutral";
  }
}

export function truncate(value: string, length: number): string {
  if (value.length <= length) return value;
  return `${value.slice(0, length)}...`;
}

export function short(value: string, length = 8): string {
  return truncate(value, length);
}

export function auditDetail(event: AuditEvent): string {
  const kind = event.kind;
  switch (kind.type) {
    case "intent_dispatched":
      return `${kind.matched_agent ?? "(no agent matched)"} · ${truncate(kind.intent_text, 90)}`;
    case "hermes_tool_invoked":
      return `${kind.tool} · run ${short(kind.run_id)}`;
    case "hermes_tool_completed":
      return `${kind.tool} · ${kind.error ? "failed" : "done"} · ${kind.duration_ms}ms`;
    case "hermes_approval_requested":
      return `run ${short(kind.run_id)} · ${kind.choices.join(", ")}`;
    case "hermes_approval_resolved":
      return `run ${short(kind.run_id)} · ${kind.choice}`;
    case "hermes_file_written":
      return `${kind.path} · ${kind.bytes} bytes`;
    case "capability_check":
      return `${kind.agent_id} · ${kind.passed ? "passed" : "missing"} · ${kind.required_actions.join(", ")}`;
    case "capability_granted":
      return `${kind.action} → ${kind.subject_display}`;
    case "intent_ignored":
      return `matched ${kind.matched_pattern}`;
    case "a2a_result_rejected":
      return `task ${short(kind.task_id)} · ${kind.reason}`;
    case "budget_exhausted":
      return `${kind.agent_display} · ${kind.tokens_remaining}/${kind.requested} credits · ${truncate(kind.intent_text, 72)}`;
    case "budget_unseeded":
      return `${kind.agent_display} · ${kind.requested} credits requested · bucket unseeded`;
    case "operator_token_rotated":
      return `${kind.peer_display} · ${kind.old_token_prefix}... → ${kind.new_token_prefix}...`;
    case "operator_token_rotation_rejected":
    case "operator_peers_list_rejected":
    case "operator_peer_revoke_rejected":
      return `${kind.peer_display} · pubkey ${short(kind.peer_pubkey_b58)} · rejected`;
    case "peer_revoked":
      return `${kind.peer_display} · token ${kind.token_prefix}...`;
    case "authentication_failed":
      return `${kind.transport} · ${kind.reason}`;
    case "a2a_sender_mismatch":
      return `${kind.peer_display} claimed ${kind.claimed_sender_display}`;
    case "a2a_recipient_rejected":
      return `${kind.sender_display} → ${kind.recipient_display} · missing ${kind.action}`;
    case "a2a_auto_retry_scheduler_scan":
      return `${kind.enabled ? "enabled" : "disabled"} · considered ${kind.considered} · requeued ${kind.requeued} · skipped ${kind.skipped}${kind.error ? ` · ${kind.error}` : ""}`;
    case "capability_revoke_rejected":
      return `sig ${short(kind.signature_b58)} · ${kind.reason}`;
  }
}

export function auditSummary(event: AuditEvent): string {
  const kind = event.kind;
  switch (kind.type) {
    case "intent_dispatched":
      if (kind.matched_agent) {
        return `Intent "${truncate(kind.intent_text, 60)}" routed to agent ${kind.matched_agent}; result hash ${kind.result_hash_hex}.`;
      }
      return `Intent "${truncate(kind.intent_text, 60)}" had no matching agent. Daemon returned the fallback response.`;
    case "capability_check":
      if (kind.passed) {
        return `Permission check passed for ${kind.agent_id}: required ${kind.required_actions.join(", ")}.`;
      }
      return `Permission check failed for ${kind.agent_id}: missing ${kind.missing_actions.join(", ")}. Required ${kind.required_actions.join(", ")}.`;
    case "capability_granted":
      return `${kind.granted_by_display} granted ${kind.action} to ${kind.subject_display}. Signed: ${short(kind.signature_b58, 12)}.`;
    case "intent_ignored":
      return `Intent dropped by .covenantignore rule "${kind.matched_pattern}".`;
    case "budget_exhausted":
      return `${kind.agent_display} exhausted its credit bucket while running ${truncate(kind.intent_text, 60)}. ${kind.tokens_remaining} remaining of ${kind.requested} requested; refill in ${kind.refill_eta_ms} ms.`;
    case "budget_unseeded":
      return `${kind.agent_display} attempted dispatch but no budget has been configured. Dispatched anyway — configure budgets to silence this warning.`;
    case "peer_revoked":
      return `Peer ${kind.peer_display} revoked. Future authenticate frames with token ${kind.token_prefix}... will be rejected.`;
    case "operator_token_rotated":
      return `Operator token rotated from ${kind.old_token_prefix}... to ${kind.new_token_prefix}... for ${kind.peer_display}.`;
    case "authentication_failed":
      return `An authenticate frame was rejected on ${kind.transport}: ${kind.reason}.`;
    case "a2a_result_rejected":
      return `An A2A task result was rejected: ${kind.reason}. Task ${short(kind.task_id)}.`;
    case "a2a_sender_mismatch":
      return `${kind.peer_display} sent an A2A frame claiming to be ${kind.claimed_sender_display}. Rejected.`;
    case "a2a_recipient_rejected":
      return `${kind.sender_display} tried to send an A2A task to ${kind.recipient_display} but lacks ${kind.action}.`;
    default:
      return auditDetail(event);
  }
}

export function eventIntentId(event: AuditEvent): string | null {
  const kind = event.kind;
  if (
    kind.type === "intent_dispatched" ||
    kind.type === "intent_ignored" ||
    kind.type === "budget_exhausted" ||
    kind.type === "budget_unseeded" ||
    kind.type === "hermes_tool_invoked" ||
    kind.type === "hermes_tool_completed" ||
    kind.type === "hermes_approval_requested" ||
    kind.type === "hermes_approval_resolved" ||
    kind.type === "hermes_file_written"
  ) {
    return kind.intent_id;
  }
  if (kind.type === "capability_check" && kind.agent_id.startsWith("memory-write:")) {
    return kind.agent_id.slice("memory-write:".length);
  }
  return null;
}

export function isReviewEvent(event: AuditEvent): boolean {
  const kind = event.kind;
  return (
    (kind.type === "capability_check" && !kind.passed) ||
    kind.type === "budget_exhausted" ||
    kind.type === "budget_unseeded" ||
    kind.type === "a2a_result_rejected" ||
    kind.type === "a2a_recipient_rejected"
  );
}

export function eventsForIntent(events: AuditEvent[], intentId: string): AuditEvent[] {
  // Rows that carry the intent_id directly: dispatch, the hermes step trail
  // (tool invoked/completed/approval), budget, ignore. These are the bulk of
  // the timeline and stream in live during an async run — before the
  // intent_dispatched row exists.
  const sorted = events.slice().sort((a, b) => a.timestamp_ms - b.timestamp_ms);
  const direct = sorted.filter((e) => eventIntentId(e) === intentId);
  const dispatchIdx = sorted.findIndex(
    (e) => e.kind.type === "intent_dispatched" && e.kind.intent_id === intentId,
  );
  // Run still in flight (no dispatch row yet): show whatever steps have landed.
  if (dispatchIdx === -1) return direct;

  // capability_check rows don't carry intent_id; correlate them by walking
  // back from the dispatch row within a small same-issuer window.
  const dispatch = sorted[dispatchIdx];
  const sameIssuer = dispatch.issuer.pubkey;
  const WINDOW_MS = 5000;
  const before: AuditEvent[] = [];
  for (let i = dispatchIdx - 1; i >= 0; i--) {
    const event = sorted[i];
    if (dispatch.timestamp_ms - event.timestamp_ms > WINDOW_MS) break;
    if (event.issuer.pubkey !== sameIssuer) continue;
    if (event.kind.type === "intent_dispatched") break;
    if (event.kind.type === "capability_check") before.unshift(event);
  }

  const seen = new Set<string>();
  return [...before, ...direct]
    .filter((e) => (seen.has(e.id) ? false : (seen.add(e.id), true)))
    .sort((a, b) => a.timestamp_ms - b.timestamp_ms);
}

export function time(ms: number): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export function dateTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function relTime(fromMs: number, nowMs: number = Date.now()): string {
  const delta = Math.max(0, Math.floor((nowMs - fromMs) / 1000));
  if (delta < 60) return `${delta}s ago`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  return `${Math.floor(delta / 86400)}d ago`;
}

export const AUDIT_KIND_LABELS: Record<AuditKind["type"], string> = {
  intent_dispatched: "intent dispatched",
  hermes_tool_invoked: "tool invoked",
  hermes_tool_completed: "tool completed",
  hermes_approval_requested: "approval requested",
  hermes_approval_resolved: "approval resolved",
  hermes_file_written: "file written",
  capability_check: "capability check",
  capability_granted: "capability granted",
  intent_ignored: "intent ignored",
  a2a_result_rejected: "a2a result rejected",
  budget_exhausted: "budget exhausted",
  budget_unseeded: "budget unseeded",
  operator_token_rotated: "operator token rotated",
  operator_token_rotation_rejected: "token rotation rejected",
  operator_peers_list_rejected: "peers list rejected",
  peer_revoked: "peer revoked",
  operator_peer_revoke_rejected: "peer revoke rejected",
  authentication_failed: "authentication failed",
  a2a_sender_mismatch: "a2a sender mismatch",
  a2a_recipient_rejected: "a2a recipient rejected",
  a2a_auto_retry_scheduler_scan: "a2a retry scan",
  capability_revoke_rejected: "capability revoke rejected",
};
