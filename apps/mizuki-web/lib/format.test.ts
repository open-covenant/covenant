import { describe, expect, it } from 'vitest';
import {
  bountyStateLabel,
  failureLabel,
  formatPercent,
  formatSolLamports,
  formatTime,
  formatUsdcAtomic,
  relativeTime,
  stateLabel,
  truncateAddress,
} from './format';

describe('format helpers', () => {
  it('formats USDC atomic units without floating point loss', () => {
    expect(formatUsdcAtomic('2000000')).toBe('2 USDC');
    expect(formatUsdcAtomic('10500000')).toBe('10.5 USDC');
  });

  it('formats native SOL fees without floating point conversion', () => {
    expect(formatSolLamports('1250000000')).toBe('1.25 SOL');
    expect(formatSolLamports('1')).toBe('0.000000001 SOL');
  });

  it('formats ratios as whole percentages', () => {
    expect(formatPercent(0.997)).toBe('100%');
  });

  it('formats receipt times in an explicit timezone', () => {
    expect(formatTime('2026-08-24T10:00:00Z')).toContain('UTC');
  });

  it('truncates long addresses', () => {
    expect(truncateAddress('1234567890abcdefghij')).toBe('12345…fghij');
  });

  it('uses relative times for stable activity copy', () => {
    const now = Date.parse('2026-08-22T12:00:00Z');
    expect(relativeTime('2026-08-22T11:30:00Z', now)).toBe('30 minutes ago');
  });

  it('turns internal failure categories into customer-facing labels', () => {
    expect(failureLabel('model_route')).toBe('Required AI service did not complete');
    expect(failureLabel('repository_validation')).toBe('Repository checks did not pass');
    expect(failureLabel()).toBe('Maintenance job not delivered');
    expect(failureLabel('unexpected_internal_code')).toBe('Maintenance job not delivered');
  });

  it('does not turn unknown internal states into public copy', () => {
    expect(stateLabel('unexpected_internal_state')).toBe('Status unavailable');
    expect(bountyStateLabel('unexpected_internal_state')).toBe('Bounty status unavailable');
    expect(bountyStateLabel('offer_refund_pending')).toBe('SOL escrow return pending');
  });
});
