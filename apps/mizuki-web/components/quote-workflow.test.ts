import { describe, expect, it } from 'vitest';
import { publicPaymentFailure } from './quote-workflow';

describe('public payment recovery messaging', () => {
  it('treats any post-sign failure as uncertain and blocks an unsafe retry', () => {
    const failure = publicPaymentFailure(
      new Error('Failed to fetch after submitting payment'),
      true,
      'quote-123',
      '2000000',
    );

    expect(failure.uncertain).toBe(true);
    expect(failure.message).toContain('Payment status could not be confirmed');
    expect(failure.message).toContain('Do not submit another payment');
    expect(failure.message).toContain('quote-123');
    expect(failure.message).not.toContain('not charged');
  });

  it('uses the preparation error only before the wallet signs', () => {
    const failure = publicPaymentFailure(
      new Error('HTTP error (403): Access forbidden'),
      false,
      'quote-123',
      '2000000',
    );

    expect(failure.uncertain).toBe(false);
    expect(failure.message).toContain('Solana payment network could not prepare the transaction');
    expect(failure.message).not.toContain('Do not submit another payment');
  });
});
