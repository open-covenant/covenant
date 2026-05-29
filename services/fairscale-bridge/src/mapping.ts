import type { AuditEvent } from './daemon.js';

export type Outcome = 'success' | 'failure' | 'neutral';

export interface ConductEvent {
  id: string;
  agent_id: string;
  agent_display: string;
  occurred_at: string;
  occurred_at_ms: number;
  source: 'covenant';
  pillar: 'work_history';
  event_type: string;
  outcome: Outcome;
  weight: number;
  summary: string;
  detail: Record<string, unknown>;
}

const NEGATIVE = new Set([
  'capability_grant_rejected',
  'capability_scope_rejected',
  'a2a_result_rejected',
  'authentication_failed',
  'a2a_sender_mismatch',
  'budget_exhausted',
  'budget_preempted',
  'peer_revoked',
  'intent_ignored',
]);

const WEIGHT: Record<string, number> = {
  intent_dispatched: 3,
  hermes_tool_completed: 2,
  capability_check: 1,
  capability_granted: 2,
  authentication_failed: 2,
  a2a_sender_mismatch: 4,
  a2a_result_rejected: 3,
  peer_revoked: 3,
};

// Free-text fields that may carry user/task content. Redacted before leaving
// the boundary; the scoring pillar needs the signal, not the payload.
const REDACT_KEYS = new Set(['intent_text', 'choices']);

function outcome(kind: AuditEvent['kind']): Outcome {
  const t = kind.type;
  if (NEGATIVE.has(t)) return 'failure';
  switch (t) {
    case 'intent_dispatched': {
      const status = String(kind.status ?? '');
      if (status === 'success' || status === 'ok') return 'success';
      if (status === 'failed' || status === 'error') return 'failure';
      return 'neutral';
    }
    case 'hermes_tool_completed':
      return kind.error ? 'failure' : 'success';
    case 'capability_check':
      return kind.passed ? 'success' : 'failure';
    case 'capability_granted':
    case 'hermes_approval_resolved':
      return 'success';
    default:
      return 'neutral';
  }
}

function weight(kind: AuditEvent['kind'], o: Outcome): number {
  const base = WEIGHT[kind.type] ?? 1;
  if (o === 'success') return base;
  if (o === 'failure') return -base;
  return 0;
}

function summary(kind: AuditEvent['kind']): string {
  switch (kind.type) {
    case 'intent_dispatched':
      return kind.matched_agent ? `intent ${kind.status} -> ${kind.matched_agent}` : `intent ${kind.status}`;
    case 'hermes_tool_invoked':
      return `tool ${kind.tool} invoked`;
    case 'hermes_tool_completed':
      return `tool ${kind.tool} ${kind.error ? 'errored' : 'ok'} in ${kind.duration_ms}ms`;
    case 'hermes_approval_requested':
      return `approval requested (${Array.isArray(kind.choices) ? kind.choices.length : 0} options)`;
    case 'capability_check':
      return `capability check ${kind.passed ? 'passed' : 'failed'}`;
    case 'capability_granted':
      return `granted ${kind.action} to ${kind.subject_display}`;
    case 'authentication_failed':
      return `auth failed (${kind.transport})`;
    case 'a2a_sender_mismatch':
      return `sender mismatch: claimed ${kind.claimed_sender_display}`;
    case 'budget_exhausted':
      return `budget exhausted for ${kind.agent_display}`;
    case 'peer_revoked':
      return `peer revoked: ${kind.peer_display}`;
    default:
      return kind.type.replace(/_/g, ' ');
  }
}

function redact(detail: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(detail)) {
    if (!REDACT_KEYS.has(k) && !k.endsWith('_text')) {
      out[k] = v;
    } else if (typeof v === 'string') {
      out[k] = `[redacted:${v.length}]`;
    } else if (Array.isArray(v)) {
      out[k] = `[redacted:${v.length} items]`;
    } else if (v == null) {
      out[k] = v;
    } else {
      out[k] = '[redacted]';
    }
  }
  return out;
}

export function toConductEvent(e: AuditEvent): ConductEvent {
  const { type, ...detail } = e.kind;
  const o = outcome(e.kind);
  return {
    id: e.id,
    agent_id: e.issuer.pubkey,
    agent_display: e.issuer.display,
    occurred_at: new Date(e.timestamp_ms).toISOString(),
    occurred_at_ms: e.timestamp_ms,
    source: 'covenant',
    pillar: 'work_history',
    event_type: type,
    outcome: o,
    weight: weight(e.kind, o),
    summary: summary(e.kind),
    detail: redact(detail),
  };
}
