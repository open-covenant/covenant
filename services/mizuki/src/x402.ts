import {
  HTTPFacilitatorClient,
  decodePaymentSignatureHeader,
  encodePaymentRequiredHeader,
  encodePaymentResponseHeader,
} from '@x402/core/http';
import { x402ResourceServer } from '@x402/core/server';
import type { PaymentPayload, PaymentRequired, PaymentRequirements } from '@x402/core/types';
import { registerExactSvmScheme } from '@x402/svm/exact/server';
import type { Config } from './config.js';
import type { Payment, Quote } from './types.js';

export const USDC_MAINNET = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
export const USDC_DECIMALS = 6;
export const SOLANA_MAINNET = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';

const PAYMENT_HEADER_MAX_BYTES = 64_000;
const MOCK_FEE_PAYER = '2wKupLR9q6wXYppw8Gr2NvWxKBUqm4PPJKkQfoxHDBg4';

export type PaymentAttempt =
  | { ok: true; payment: Payment; responseHeader?: string }
  | { ok: false; challenge: PaymentRequired; reason?: string };

export class Payments {
  private readonly server?: x402ResourceServer;
  private readonly facilitator?: HTTPFacilitatorClient;
  private initialized?: Promise<void>;

  constructor(private readonly config: Config) {
    if (config.paymentMode !== 'live') return;
    if (!config.payTo) throw new Error('MIZUKI_PAY_TO is required in live payment mode');

    const facilitator = new HTTPFacilitatorClient({ url: config.facilitator, timeoutMs: 15_000 });
    const server = new x402ResourceServer(facilitator);
    registerExactSvmScheme(server, { networks: [SOLANA_MAINNET] });
    this.facilitator = facilitator;
    this.server = server;
  }

  async initialize(): Promise<void> {
    if (!this.server) return;
    this.initialized ??= this.server.initialize();
    await this.initialized;
  }

  async readiness(): Promise<void> {
    if (this.config.paymentMode === 'mock') return;
    const supported = await this.facilitator!.getSupported();
    if (!isSupportedResponse(supported)) throw new Error('facilitator evidence is invalid');
    const route = supported.kinds.some(
      (kind) =>
        kind.x402Version === 2 && kind.scheme === 'exact' && kind.network === SOLANA_MAINNET,
    );
    const signers = supported.signers[SOLANA_MAINNET];
    if (!route || !Array.isArray(signers) || signers.length === 0) {
      throw new Error('facilitator does not support the required mainnet route');
    }
  }

  async challenge(quote: Quote): Promise<PaymentRequired> {
    if (this.config.paymentMode === 'mock') return mockChallenge(quote, this.config);
    const requirements = await this.requirements(quote);
    return this.server!.createPaymentRequiredResponse(
      [requirements],
      this.resourceInfo(quote),
      'Payment required',
    );
  }

  async settle(
    quote: Quote,
    signature: string | undefined,
    persist: (payment: Payment) => void | Promise<void> = () => {},
  ): Promise<PaymentAttempt> {
    if (!signature) return { ok: false, challenge: await this.challenge(quote) };
    if (this.config.paymentMode === 'mock') {
      const match = signature.match(/^mock:([1-9A-HJ-NP-Za-km-z]{32,44}):([A-Za-z0-9_-]{8,128})$/);
      if (!match) {
        return {
          ok: false,
          challenge: await this.challenge(quote),
          reason: 'invalid mock payment',
        };
      }
      const authorized: Payment = {
        payer: match[1]!,
        transaction: 'pending',
        amountAtomic: quote.priceAtomic,
        signature,
      };
      await persist(authorized);
      return {
        ok: true,
        payment: { ...authorized, transaction: match[2]! },
      };
    }

    let payload: PaymentPayload;
    try {
      payload = this.decode(signature);
    } catch {
      return {
        ok: false,
        challenge: await this.challenge(quote),
        reason: 'invalid payment signature header',
      };
    }

    const requirements = await this.requirements(quote);
    if (payload.resource?.url !== this.resource(quote)) {
      return {
        ok: false,
        challenge: await this.challenge(quote),
        reason: 'payment resource does not match quote',
      };
    }
    const accepted = this.server!.findMatchingRequirements([requirements], payload);
    if (!accepted || !this.matchesQuote(accepted, quote)) {
      return {
        ok: false,
        challenge: await this.challenge(quote),
        reason: 'payment requirements do not match quote',
      };
    }

    const verified = await this.server!.verifyPayment(payload, accepted);
    if (!verified.isValid) {
      return {
        ok: false,
        challenge: await this.challenge(quote),
        reason: verified.invalidReason ?? verified.invalidMessage ?? 'payment verification failed',
      };
    }
    if (!verified.payer) throw new Error('facilitator verification returned no payer');
    await persist({
      payer: verified.payer,
      transaction: 'pending',
      amountAtomic: quote.priceAtomic,
      signature,
    });

    const settled = await this.server!.settlePayment(payload, accepted);
    if (!settled.success || !settled.payer || !settled.transaction) {
      throw new Error(settled.errorReason ?? settled.errorMessage ?? 'payment settlement failed');
    }
    if (settled.payer !== verified.payer) {
      throw new Error('settlement payer does not match verification');
    }
    const payment: Payment = {
      payer: settled.payer,
      transaction: settled.transaction,
      amountAtomic: settled.amount ?? quote.priceAtomic,
      signature,
    };
    if (payment.amountAtomic !== quote.priceAtomic) {
      throw new Error('settled payment amount does not match quote');
    }
    return {
      ok: true,
      payment,
      responseHeader: encodePaymentResponseHeader(settled),
    };
  }

  async retrySettlement(quote: Quote, payment: Payment): Promise<Payment> {
    if (!payment.signature) throw new Error('stored payment signature is unavailable');
    if (this.config.paymentMode === 'mock') {
      const transaction = payment.signature.split(':').at(-1);
      if (!transaction) throw new Error('invalid stored mock payment');
      return { ...payment, transaction };
    }

    await this.ready();
    const payload = this.decode(payment.signature);
    if (
      payload.resource?.url !== this.resource(quote) ||
      !this.matchesQuote(payload.accepted, quote)
    ) {
      throw new Error('stored payment proof does not match quote');
    }
    const settled = await this.server!.settlePayment(payload, payload.accepted);
    if (!settled.success || !settled.payer || !settled.transaction) {
      throw new Error(
        settled.errorReason ?? settled.errorMessage ?? 'payment settlement retry failed',
      );
    }
    if (settled.payer !== payment.payer) throw new Error('settlement payer changed during retry');
    const amountAtomic = settled.amount ?? quote.priceAtomic;
    if (amountAtomic !== quote.priceAtomic) {
      throw new Error('settled payment amount does not match quote');
    }
    return { ...payment, transaction: settled.transaction, amountAtomic };
  }

  private async requirements(quote: Quote): Promise<PaymentRequirements> {
    await this.ready();
    const requirements = await this.server!.buildPaymentRequirements({
      scheme: 'exact',
      network: SOLANA_MAINNET,
      payTo: this.config.payTo,
      price: { asset: USDC_MAINNET, amount: quote.priceAtomic },
      maxTimeoutSeconds: 300,
      extra: {
        description: `${quote.class} maintenance job for ${quote.owner}/${quote.repo}#${quote.issueNumber}`,
      },
    });
    const accepted = requirements.find((candidate) => this.matchesQuote(candidate, quote));
    if (!accepted) throw new Error('facilitator did not return the required mainnet USDC route');
    return accepted;
  }

  private async ready(): Promise<void> {
    await this.initialize();
  }

  private decode(signature: string): PaymentPayload {
    if (Buffer.byteLength(signature, 'utf8') > PAYMENT_HEADER_MAX_BYTES) {
      throw new Error('payment signature header is too large');
    }
    const payload = decodePaymentSignatureHeader(signature);
    if (payload.x402Version !== 2) throw new Error('only x402 v2 is accepted');
    return payload;
  }

  private matchesQuote(requirements: PaymentRequirements, quote: Quote): boolean {
    return (
      requirements.scheme === 'exact' &&
      requirements.network === SOLANA_MAINNET &&
      requirements.asset === USDC_MAINNET &&
      requirements.payTo === this.config.payTo &&
      requirements.amount === quote.priceAtomic &&
      requirements.maxTimeoutSeconds === 300
    );
  }

  private resourceInfo(quote: Quote) {
    return {
      url: this.resource(quote),
      description: 'Mizuki software maintenance job',
      mimeType: 'application/json',
      serviceName: 'Mizuki',
      tags: ['software-maintenance', 'github'],
    };
  }

  private resource(quote: Quote): string {
    return `${this.config.publicBaseUrl.replace(/\/$/, '')}/v1/jobs?quote_id=${quote.id}`;
  }
}

function isSupportedResponse(value: unknown): value is {
  kinds: Array<{ x402Version: number; scheme: string; network: string }>;
  extensions: string[];
  signers: Record<string, string[]>;
} {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const candidate = value as Record<string, unknown>;
  if (!Array.isArray(candidate.kinds) || !Array.isArray(candidate.extensions)) return false;
  if (
    typeof candidate.signers !== 'object' ||
    candidate.signers === null ||
    Array.isArray(candidate.signers)
  ) {
    return false;
  }
  if (!candidate.extensions.every((extension) => typeof extension === 'string')) return false;
  if (
    !Object.values(candidate.signers as Record<string, unknown>).every(
      (signers) =>
        Array.isArray(signers) && signers.every((signer) => typeof signer === 'string' && signer),
    )
  ) {
    return false;
  }
  return candidate.kinds.every((kind) => {
    if (typeof kind !== 'object' || kind === null || Array.isArray(kind)) return false;
    const fields = kind as Record<string, unknown>;
    return (
      Number.isInteger(fields.x402Version) &&
      typeof fields.scheme === 'string' &&
      fields.scheme.length > 0 &&
      typeof fields.network === 'string' &&
      fields.network.length > 0
    );
  });
}

export function paymentRequiredHeader(challenge: PaymentRequired): string {
  return encodePaymentRequiredHeader(challenge);
}

function mockChallenge(quote: Quote, config: Config): PaymentRequired {
  return {
    x402Version: 2,
    resource: {
      url: `${config.publicBaseUrl.replace(/\/$/, '')}/v1/jobs?quote_id=${quote.id}`,
      description: 'Mizuki software maintenance job',
      mimeType: 'application/json',
      serviceName: 'Mizuki',
      tags: ['software-maintenance', 'github'],
    },
    accepts: [
      {
        scheme: 'exact',
        network: SOLANA_MAINNET,
        amount: quote.priceAtomic,
        payTo: config.payTo || '11111111111111111111111111111111',
        maxTimeoutSeconds: 300,
        asset: USDC_MAINNET,
        extra: { feePayer: MOCK_FEE_PAYER },
      },
    ],
    error: 'Payment required',
  };
}
