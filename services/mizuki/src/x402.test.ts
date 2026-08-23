import { createServer } from 'node:http';
import { encodePaymentSignatureHeader } from '@x402/core/http';
import { describe, expect, it } from 'vitest';
import { loadConfig } from './config.js';
import type { Quote } from './types.js';
import { Payments, SOLANA_MAINNET } from './x402.js';

const quote: Quote = {
  id: '4f8f70af-e8f4-4e0a-9fc7-7d88f6e281ab',
  issueUrl: 'https://github.com/example/project/issues/1',
  owner: 'example',
  repo: 'project',
  issueNumber: 1,
  issueTitle: 'Fix docs',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: [],
  expiresAt: '2099-01-01T00:00:00Z',
};

describe('Payments mock mode', () => {
  it('returns a v2 mainnet USDC challenge and deterministic settlement', async () => {
    const payments = new Payments(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }));
    const challenge = await payments.challenge(quote);
    expect(challenge.x402Version).toBe(2);
    expect(challenge.accepts[0]).toMatchObject({ amount: '2000000', scheme: 'exact' });

    let persistedTransaction: string | undefined;
    const result = await payments.settle(
      quote,
      `mock:${'1'.repeat(32)}:transaction_123`,
      (payment) => {
        persistedTransaction = payment.transaction;
      },
    );
    expect(persistedTransaction).toBe('pending');
    expect(result).toMatchObject({
      ok: true,
      payment: { amountAtomic: '2000000', transaction: 'transaction_123' },
    });
  });

  it('does not accept malformed proof', async () => {
    const payments = new Payments(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }));
    expect(await payments.settle(quote, 'paid-trust-me')).toMatchObject({ ok: false });
  });
});

describe('Payments live x402 boundary', () => {
  it('uses one v2 requirement for verification, durable authorization, and settlement', async () => {
    const treasury = '2'.repeat(32);
    const feePayer = '2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4';
    const payer = '4'.repeat(32);
    let signers = { 'solana:*': [feePayer] };
    const requests: Array<{ path: string; body?: Record<string, unknown> }> = [];
    const facilitator = createServer(async (request, response) => {
      let body: Record<string, unknown> | undefined;
      if (request.method === 'POST') {
        const chunks: Buffer[] = [];
        for await (const chunk of request) chunks.push(Buffer.from(chunk));
        body = JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, unknown>;
      }
      requests.push({ path: request.url ?? '', ...(body ? { body } : {}) });
      response.setHeader('content-type', 'application/json');
      if (request.url === '/supported') {
        response.end(
          JSON.stringify({
            kinds: [
              {
                x402Version: 2,
                scheme: 'exact',
                network: SOLANA_MAINNET,
                extra: { feePayer },
              },
            ],
            extensions: [],
            signers,
          }),
        );
        return;
      }
      if (request.url === '/verify') {
        response.end(JSON.stringify({ isValid: true, payer }));
        return;
      }
      if (request.url === '/settle') {
        response.end(
          JSON.stringify({
            success: true,
            payer,
            transaction: 'settlement_signature',
            network: SOLANA_MAINNET,
            amount: quote.priceAtomic,
          }),
        );
        return;
      }
      response.statusCode = 404;
      response.end('{}');
    });
    await new Promise<void>((resolve) => facilitator.listen(0, '127.0.0.1', resolve));
    const address = facilitator.address();
    if (!address || typeof address === 'string') throw new Error('facilitator did not bind');

    try {
      const payments = new Payments(
        loadConfig({
          MIZUKI_PAYMENT_MODE: 'live',
          MIZUKI_PAY_TO: treasury,
          MIZUKI_PUBLIC_BASE_URL: 'https://mizuki.example',
          MIZUKI_X402_FACILITATOR: `http://127.0.0.1:${address.port}`,
        }),
      );
      const challenge = await payments.challenge(quote);
      expect(challenge.accepts).toHaveLength(1);
      expect(challenge.accepts[0]).toMatchObject({
        scheme: 'exact',
        network: SOLANA_MAINNET,
        asset: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
        amount: quote.priceAtomic,
        payTo: treasury,
        extra: { feePayer },
      });
      const wrongResourceSignature = encodePaymentSignatureHeader({
        x402Version: 2,
        resource: { ...challenge.resource, url: 'https://mizuki.example/v1/jobs?quote_id=other' },
        accepted: challenge.accepts[0]!,
        payload: { transaction: 'signed_transaction' },
      });
      await expect(payments.settle(quote, wrongResourceSignature)).resolves.toMatchObject({
        ok: false,
        reason: 'payment resource does not match quote',
      });
      expect(requests.map((request) => request.path)).toEqual(['/supported']);

      const signature = encodePaymentSignatureHeader({
        x402Version: 2,
        resource: challenge.resource,
        accepted: challenge.accepts[0]!,
        payload: { transaction: 'signed_transaction' },
      });
      let authorizedTransaction: string | undefined;
      const settled = await payments.settle(quote, signature, (payment) => {
        authorizedTransaction = payment.transaction;
      });
      expect(authorizedTransaction).toBe('pending');
      expect(settled).toMatchObject({
        ok: true,
        payment: {
          payer,
          amountAtomic: quote.priceAtomic,
          transaction: 'settlement_signature',
        },
      });
      expect(requests.map((request) => request.path)).toEqual(['/supported', '/verify', '/settle']);
      expect(requests[1]?.body).toMatchObject({
        x402Version: 2,
        paymentRequirements: challenge.accepts[0],
      });
      await expect(payments.readiness()).resolves.toBeUndefined();
      expect(requests.map((request) => request.path)).toEqual([
        '/supported',
        '/verify',
        '/settle',
        '/supported',
      ]);

      signers = { 'solana:devnet': [feePayer] };
      await expect(payments.readiness()).rejects.toThrow(
        'facilitator does not support the required mainnet route',
      );

      signers = { 'solana:*': ['5'.repeat(32)] };
      await expect(payments.readiness()).rejects.toThrow(
        'facilitator does not support the required mainnet route',
      );
    } finally {
      await new Promise<void>((resolve, reject) =>
        facilitator.close((error) => (error ? reject(error) : resolve())),
      );
    }
  });
});
