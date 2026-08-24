import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { demoTreasury } from '@/lib/demo';
import { TreasurySnapshot } from './treasury-snapshot';

describe('TreasurySnapshot', () => {
  it('labels verified reserve custody and planning records as separate evidence', () => {
    const html = renderToStaticMarkup(<TreasurySnapshot treasury={demoTreasury} />);

    expect(html).toContain('Verified refund reserve balance');
    expect(html).toContain('Service-record net flow');
    expect(html).toContain('not a wallet balance');
    expect(html).toContain('Planned improvement allocation');
  });

  it('does not turn missing signer evidence into a verified custody claim', () => {
    const treasury = {
      ...demoTreasury,
      refundProtection: {
        ...demoTreasury.refundProtection,
        status: 'unavailable' as const,
        source: null,
        finalizedBalanceAtomic: null,
        signerOutstandingLiabilityAtomic: null,
        unencumberedBalanceAtomic: null,
        newIntakeCapacityAtomic: null,
        remainingDailyLimitUsdCents: null,
        liabilityReconciled: null,
        liabilitiesBacked: null,
        checkedAt: null,
      },
    };

    const html = renderToStaticMarkup(<TreasurySnapshot treasury={treasury} />);

    expect(html).toContain('Finalized reserve records are unavailable');
    expect(html).not.toContain('Verified refund reserve balance');
    expect(html).not.toContain('Reserve-cleared');
  });
});
