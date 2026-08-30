import { describe, expect, it } from 'vitest';

import {
  errorCode,
  errorMessage,
  formatDuration,
  formatElapsed,
  formatUsdc,
  formatVram,
  isValidAccessUrl,
  launchRecovery,
  launchRecoveryCopy,
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

  it('counts elapsed time up without rounding away seconds', () => {
    expect(formatElapsed(9)).toBe('0:09');
    expect(formatElapsed(185)).toBe('3:05');
    expect(formatElapsed(3_725)).toBe('1:02:05');
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

  it('extracts a command code only when the runtime reports one', () => {
    expect(errorCode({ code: 'stale_offer', message: 'Offer is gone.' })).toBe('stale_offer');
    expect(errorCode(new Error('Offer is gone.'))).toBeNull();
    expect(errorCode({ code: '  ' })).toBeNull();
    expect(errorCode(null)).toBeNull();
  });
});

describe('launch recovery', () => {
  it('requotes when the reserved GPU is gone', () => {
    expect(launchRecovery({ code: 'stale_offer', message: 'gone' })).toBe('requote');
    expect(launchRecovery({ code: 'no_compatible_offer', message: 'gone' })).toBe('requote');
    expect(launchRecoveryCopy.requote).toContain('taken');
    expect(launchRecoveryCopy.requote).toContain('fresh quote');
  });

  it('routes rejected tokens and outdated plans to their own remedies', () => {
    expect(launchRecovery({ code: 'unauthorized' })).toBe('reauthenticate');
    expect(launchRecovery({ code: 'invalid_launch_plan' })).toBe('outdated');
    expect(launchRecoveryCopy.outdated).toContain('latest release');
  });

  it('reports every other failure as-is', () => {
    expect(launchRecovery({ code: 'spend_cap_exceeded' })).toBe('report');
    expect(launchRecovery(new Error('network down'))).toBe('report');
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
