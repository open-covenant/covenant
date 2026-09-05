import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AssetRoutedFacilitator } from './facilitator-router.js';

const USDC = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const MIZUKI = 'DwquZcs2JtPe2w9xfyqF9wDnySQXLBHTMawusJ8Uk1mi';

function spy(name: string, calls: string[]) {
  return {
    verify: async () => {
      calls.push(`${name}.verify`);
      return { isValid: true, payer: 'p' };
    },
    settle: async () => {
      calls.push(`${name}.settle`);
      return { success: true, transaction: `${name}-tx`, network: 'solana', payer: 'p' };
    },
    getSupported: async () => {
      calls.push(`${name}.getSupported`);
      return { kinds: [{ scheme: 'exact', network: 'solana', x402Version: 2 }] };
    },
  } as never;
}

const router = (calls: string[]) =>
  new AssetRoutedFacilitator({
    primary: spy('coinbase', calls),
    fallback: spy('self', calls),
    primaryAssets: [USDC],
  });

test('sends USDC to Coinbase, so the resource stays in the Bazaar', async () => {
  const calls: string[] = [];
  await router(calls).settle({} as never, { asset: USDC } as never);
  assert.deepEqual(calls, ['coinbase.settle']);
});

test('sends a token Coinbase refuses to our own facilitator', async () => {
  const calls: string[] = [];
  await router(calls).settle({} as never, { asset: MIZUKI } as never);
  assert.deepEqual(calls, ['self.settle']);
});

test('verifies and settles the same payment at the same facilitator', async () => {
  // Only the facilitator that verified holds the fee payer the payer signed
  // against, so splitting these across facilitators cannot settle.
  const calls: string[] = [];
  const r = router(calls);
  await r.verify({} as never, { asset: MIZUKI } as never);
  await r.settle({} as never, { asset: MIZUKI } as never);
  assert.deepEqual(calls, ['self.verify', 'self.settle']);
});

test('reports supported kinds from the primary, which is what the Bazaar reads', async () => {
  const calls: string[] = [];
  await router(calls).getSupported();
  assert.deepEqual(calls, ['coinbase.getSupported']);
});

test('routes an unknown or missing asset to the fallback rather than assuming', async () => {
  for (const requirements of [{}, { asset: 'something-else' }, { asset: 42 }]) {
    const calls: string[] = [];
    await router(calls).settle({} as never, requirements as never);
    assert.deepEqual(calls, ['self.settle']);
  }
});
