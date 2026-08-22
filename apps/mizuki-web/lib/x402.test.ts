import { afterEach, describe, expect, it } from 'vitest';
import { parsePaymentTerms, selectPaymentRequirements } from './x402';

const network = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
const asset = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const payTo = '2'.repeat(32);
const feePayer = '3'.repeat(32);
const requirements = {
  scheme: 'exact',
  network,
  asset,
  amount: '2000000',
  payTo,
  maxTimeoutSeconds: 300,
  extra: { feePayer },
} as const;

afterEach(() => {
  delete process.env.NEXT_PUBLIC_SOLANA_NETWORK;
});

describe('x402 quote policy', () => {
  it('selects the one route that exactly matches the accepted quote', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(selectPaymentRequirements(2, [requirements], terms)).toEqual(requirements);
  });

  it.each([
    ['amount', { ...requirements, amount: '10000000' }],
    ['asset', { ...requirements, asset: '5'.repeat(32) }],
    ['network', { ...requirements, network: 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1' }],
  ])('rejects a changed %s before the wallet signs', (_field, changed) => {
    expect(() =>
      parsePaymentTerms({ x402Version: 2, accepts: [changed] }, requirements.amount),
    ).toThrow('does not match');
  });

  it('rejects a recipient changed after quote acceptance', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(() =>
      selectPaymentRequirements(2, [{ ...requirements, payTo: '4'.repeat(32) }], terms),
    ).toThrow('does not match');
  });

  it('rejects ambiguous matching routes', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(() => selectPaymentRequirements(2, [requirements, requirements], terms)).toThrow(
      'does not match',
    );
  });
});
