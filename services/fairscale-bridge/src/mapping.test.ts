import { describe, expect, it } from 'vitest';
import { toConductEvent } from './mapping.js';
import type { AuditEvent } from './daemon.js';

const base = (kind: AuditEvent['kind']): AuditEvent => ({
  id: '11111111-1111-1111-1111-111111111111',
  timestamp_ms: 1_700_000_000_000,
  issuer: { display: 'demo@host', pubkey: 'Bzk58Pubkey' },
  kind,
});

describe('toConductEvent', () => {
  it('maps issuer to agent identity and timestamp to iso', () => {
    const c = toConductEvent(base({ type: 'intent_dispatched', status: 'success', intent_text: 'ship it' }));
    expect(c.agent_id).toBe('Bzk58Pubkey');
    expect(c.agent_display).toBe('demo@host');
    expect(c.occurred_at).toBe(new Date(1_700_000_000_000).toISOString());
    expect(c.pillar).toBe('work_history');
    expect(c.source).toBe('covenant');
  });

  it('scores a successful intent positively and a failed one negatively', () => {
    const ok = toConductEvent(base({ type: 'intent_dispatched', status: 'success', intent_text: 'x' }));
    const bad = toConductEvent(base({ type: 'intent_dispatched', status: 'failed', intent_text: 'x' }));
    expect(ok.outcome).toBe('success');
    expect(ok.weight).toBeGreaterThan(0);
    expect(bad.outcome).toBe('failure');
    expect(bad.weight).toBeLessThan(0);
  });

  it('classifies known negative event types as failures', () => {
    const c = toConductEvent(base({ type: 'authentication_failed', transport: 'http', reason: 'bad token' }));
    expect(c.outcome).toBe('failure');
    expect(c.summary).toContain('auth failed');
  });

  it('uses tool error flag for hermes completion outcome', () => {
    const ok = toConductEvent(base({ type: 'hermes_tool_completed', tool: 'fs', error: false, duration_ms: 12 }));
    const err = toConductEvent(base({ type: 'hermes_tool_completed', tool: 'fs', error: true, duration_ms: 12 }));
    expect(ok.outcome).toBe('success');
    expect(err.outcome).toBe('failure');
  });

  it('strips the type tag into event_type and keeps structural fields as detail', () => {
    const c = toConductEvent(base({ type: 'capability_check', passed: true, agent_id: 'a', required_actions: [], missing_actions: [] }));
    expect(c.event_type).toBe('capability_check');
    expect(c.detail).not.toHaveProperty('type');
    expect(c.detail).toMatchObject({ passed: true, agent_id: 'a' });
  });

  it('redacts free-text content but keeps the field and its length', () => {
    const c = toConductEvent(base({ type: 'intent_dispatched', status: 'success', intent_text: 'transfer all funds to X' }));
    expect(c.detail.intent_text).toBe('[redacted:23]');
    expect(c.summary).toBe('intent success');
    expect(JSON.stringify(c)).not.toContain('transfer all funds');
  });

  it('redacts array-valued content (approval choices) by count', () => {
    const c = toConductEvent(base({ type: 'hermes_approval_requested', intent_id: 'i', run_id: 'r', choices: ['yes', 'no'] }));
    expect(c.detail.choices).toBe('[redacted:2 items]');
    expect(c.summary).toBe('approval requested (2 options)');
  });

  it('defaults unknown event types to neutral with humanized summary', () => {
    const c = toConductEvent(base({ type: 'operator_token_rotated', old_token_prefix: 'aa', new_token_prefix: 'bb' }));
    expect(c.outcome).toBe('neutral');
    expect(c.weight).toBe(0);
    expect(c.summary).toBe('operator token rotated');
  });

  // The daemon writes status="ok"/"error"/"no_match" for IntentDispatched — it
  // never emits the "success"/"failed" spelling the cases above use — so the
  // alias arms below are the ones every real intent is scored through.
  it('scores the ok/error/no_match intent statuses the daemon actually emits', () => {
    const ok = toConductEvent(base({ type: 'intent_dispatched', status: 'ok', intent_text: 'x' }));
    expect(ok.outcome).toBe('success');
    expect(ok.weight).toBe(3);

    const err = toConductEvent(base({ type: 'intent_dispatched', status: 'error', intent_text: 'x' }));
    expect(err.outcome).toBe('failure');
    expect(err.weight).toBe(-3);

    const noMatch = toConductEvent(base({ type: 'intent_dispatched', status: 'no_match', intent_text: 'x' }));
    expect(noMatch.outcome).toBe('neutral');
    expect(noMatch.weight).toBe(0);
  });

  it('scores a capability check by its passed flag', () => {
    const passed = toConductEvent(base({ type: 'capability_check', passed: true, agent_id: 'a', required_actions: [], missing_actions: [] }));
    expect(passed.outcome).toBe('success');
    expect(passed.weight).toBe(1);

    const denied = toConductEvent(base({ type: 'capability_check', passed: false, agent_id: 'a', required_actions: ['memory.write'], missing_actions: ['memory.write'] }));
    expect(denied.outcome).toBe('failure');
    expect(denied.weight).toBe(-1);
  });
});
