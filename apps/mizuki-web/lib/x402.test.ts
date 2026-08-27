import { afterEach, describe, expect, it } from 'vitest';
import { parsePaymentTerms, paymentPreparationError, selectPaymentRequirements } from './x402';

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

  it('explains an unfunded wallet without exposing SDK internals', () => {
    expect(paymentPreparationError(new Error('insufficient token balance'), '2 USDC')).toBe(
      'Your connected wallet does not have enough USDC on Solana to pay the 2 USDC quote. Add USDC to this wallet and try again. No payment or job was created.',
    );
  });

  it('does not expose spend-control configuration to customers', () => {
    const message = paymentPreparationError(
      new Error('All payment requirements were rejected by spendControls.maxAmountPerPayment'),
      '2 USDC',
    );
    expect(message).toContain('Workbench could not authorize this quote amount');
    expect(message).not.toContain('spendControls');
    expect(message).not.toContain('maxAmountPerPayment');
  });

  it.each([400, 403])(
    'reports an unavailable browser RPC after HTTP %s without blaming the wallet balance',
    (status) => {
      const message = paymentPreparationError(
        new Error(`Failed to create payment payload: HTTP error (${status}): RPC unavailable`),
        '2 USDC',
      );

      expect(message).toContain('Solana payment network could not prepare the transaction');
      expect(message).not.toContain('balance');
      expect(message).not.toContain(String(status));
    },
  );

  it('does not claim an unknown preparation error means insufficient funds', () => {
    const message = paymentPreparationError(new Error('unexpected wallet adapter error'), '2 USDC');

    expect(message).toContain('Reconnect the wallet');
    expect(message).not.toContain('at least 2 USDC');
  });
});
