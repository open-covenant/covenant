import { describe, expect, it } from 'vitest';

import {
  formatDuration,
  errorMessage,
  formatUsdc,
  formatVram,
  isValidAccessUrl,
  showPrivateBetaAccess,
  shortId,
} from './domain';

describe('compute formatting', () => {
  it('formats USDC-denominated allowances from micros', () => {
    expect(formatUsdc(500_000)).toBe('$0.50');
    expect(formatUsdc(25_000)).toBe('$0.025');
  });

  it('formats bounded durations and VRAM', () => {
    expect(formatDuration(28)).toBe('28s');
    expect(formatDuration(1_800)).toBe('30 min');
    expect(formatDuration(5_400)).toBe('1h 30m');
    expect(formatVram(24_576)).toBe('24 GB');
  });

  it('shortens only long identifiers', () => {
    expect(shortId('job-1')).toBe('job-1');
    expect(shortId('job-0123456789abcdef')).toBe('job-012…9abcdef');
  });
});

describe('native command errors', () => {
  it('extracts a safe command message', () => {
    expect(errorMessage({ code: 'provider_unreachable', message: 'Control plane is offline.' })).toBe(
      'Control plane is offline.',
    );
    expect(errorMessage({ code: 'provider_unreachable' })).toBe(
      'The runtime returned an unexpected error.',
    );
  });
});

describe('access URL policy', () => {
  it('accepts secure remote URLs and loopback development URLs', () => {
    expect(isValidAccessUrl('https://session.example.test')).toBe(true);
    expect(isValidAccessUrl('http://127.0.0.1:8888')).toBe(true);
  });

  it('rejects insecure remote and non-web URLs', () => {
    expect(isValidAccessUrl('http://session.example.test')).toBe(false);
    expect(isValidAccessUrl('javascript:alert(1)')).toBe(false);
    expect(isValidAccessUrl('not a url')).toBe(false);
  });
});

describe('private beta access', () => {
  it('shows token controls only when native auth needs attention or is session-scoped', () => {
    const status = {
      state: 'degraded' as const,
      endpoint_label: 'compute.example',
      message: 'Access token required.',
      authentication: { source: 'none' as const },
      token_required: true,
    };

    expect(showPrivateBetaAccess(status, false)).toBe(true);
    expect(
      showPrivateBetaAccess(
        {
          ...status,
          state: 'connected',
          authentication: { source: 'session' },
          token_required: false,
        },
        false,
      ),
    ).toBe(true);
    expect(showPrivateBetaAccess(status, true)).toBe(false);
  });
});
