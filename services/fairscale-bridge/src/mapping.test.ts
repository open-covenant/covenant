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

  it('strips the type tag into event_type and keeps the rest as detail', () => {
    const c = toConductEvent(base({ type: 'capability_check', passed: true, agent_id: 'a', required_actions: [], missing_actions: [] }));
    expect(c.event_type).toBe('capability_check');
    expect(c.detail).not.toHaveProperty('type');
    expect(c.detail).toMatchObject({ passed: true, agent_id: 'a' });
  });

  it('defaults unknown event types to neutral with humanized summary', () => {
    const c = toConductEvent(base({ type: 'operator_token_rotated', old_token_prefix: 'aa', new_token_prefix: 'bb' }));
    expect(c.outcome).toBe('neutral');
    expect(c.weight).toBe(0);
    expect(c.summary).toBe('operator token rotated');
  });
});
