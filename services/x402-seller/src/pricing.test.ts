import assert from 'node:assert/strict';
import { test } from 'node:test';

/**
 * Mirrors mizukiPrice in server.ts. Kept as a copy because the server reads its
 * configuration at import time and starts listening, which a unit test must not do.
 *
 * usdcAmount is micro-USDC. usdNanosPerToken is nano-USDC per whole token,
 * because MIZUKI trades below one micro-USDC and micro precision would round the
 * rate down and overcharge the payer.
 */
function mizukiAmount(
  usdcAmount: string,
  usdNanosPerToken: string,
  discountBps = 2000,
  decimals = 6,
): bigint {
  const rate = BigInt(usdNanosPerToken);
  const discountedNanos = (BigInt(usdcAmount) * 1_000n * BigInt(10_000 - discountBps)) / 10_000n;
  const scale = 10n ** BigInt(decimals);
  const amount = (discountedNanos * scale + rate - 1n) / rate;
  return amount > 0n ? amount : 1n;
}

/** The live rate on 2026-09-04: 0.000006425 USDC per MIZUKI. */
const LIVE_RATE = '6425';

test('prices the assess call in MIZUKI at the live rate', () => {
  // 1000 micro-USDC of work, 20% off, is 800 micro-USDC = 800000 nano-USDC.
  // 800000 nano / 6425 = 124.5136186... tokens, rounded up.
  const amount = mizukiAmount('1000', LIVE_RATE);
  assert.equal(amount, 124_513_619n);
  assert.ok(amount * 6425n >= 800_000n * 1_000_000n, 'never below the discounted price');
  assert.equal(Number(amount) / 1e6 > 124 && Number(amount) / 1e6 < 125, true);
});

test('sub-micro rates are not rounded away', () => {
  // The whole reason for nano precision: 6425 nano is 6.425 micro. Truncating
  // the rate to 6 micro would overcharge the payer by about seven percent.
  const honest = mizukiAmount('1000', '6425');
  const truncated = mizukiAmount('1000', '6000');
  assert.ok(truncated > honest);
  assert.ok(Number(truncated - honest) / Number(honest) > 0.06);
});

test('a cheaper token costs proportionally more of it', () => {
  assert.equal(mizukiAmount('1000', '1000'), mizukiAmount('1000', '4000') * 4n);
});

test('rounds up, so rounding never charges less than the discounted price', () => {
  const amount = mizukiAmount('333', '9');
  assert.ok(amount * 9n >= 266n * 1_000n * 1_000_000n);
});

test('never quotes zero, however cheap the call', () => {
  assert.equal(mizukiAmount('1', '999999999999999'), 1n);
});

test('a zero discount matches the USDC value exactly', () => {
  // 2000 micro-USDC at 2000 nano per token is 1000 whole tokens.
  assert.equal(mizukiAmount('2000', '2000', 0), 1_000_000_000n);
});

test('honours a non-standard token decimals', () => {
  assert.equal(mizukiAmount('2000', '2000', 0, 9), 1_000_000_000_000n);
});
