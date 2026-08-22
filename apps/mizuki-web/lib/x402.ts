'use client';

import {
  address,
  getTransactionDecoder,
  getTransactionEncoder,
  type SignatureDictionary,
  type TransactionSigner,
} from '@solana/kit';
import type { SolanaSignTransactionFeature } from '@solana/wallet-standard-features';
import type { WalletAccount } from '@wallet-standard/base';
import { wrapFetchWithPaymentFromConfig, type PaymentRequirements } from '@x402/fetch';
import { ExactSvmScheme } from '@x402/svm/exact/client';

const SOLANA_MAINNET = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
const SOLANA_DEVNET = 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
const USDC_MAINNET = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const USDC_DEVNET = '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU';

export type PaymentTerms = {
  amount: string;
  asset: string;
  network: typeof SOLANA_MAINNET | typeof SOLANA_DEVNET;
  payTo: string;
};

export function createPaymentFetch(input: {
  account: WalletAccount;
  feature: SolanaSignTransactionFeature['solana:signTransaction'];
  quotePayment: unknown;
  quoteAmount: string;
}): typeof fetch {
  const terms = parsePaymentTerms(input.quotePayment, input.quoteAmount);
  const signer = walletSigner(input.account, input.feature, terms.network);
  const rpcUrl = process.env.NEXT_PUBLIC_SOLANA_RPC_URL;
  const scheme = new ExactSvmScheme(signer, rpcUrl ? { rpcUrl } : undefined);

  return wrapFetchWithPaymentFromConfig(fetch, {
    schemes: [{ network: terms.network, client: scheme }],
    paymentRequirementsSelector: (version, requirements) =>
      selectPaymentRequirements(version, requirements, terms),
  });
}

export function parsePaymentTerms(value: unknown, quoteAmount: string): PaymentTerms {
  if (!isRecord(value) || value.x402Version !== 2 || !Array.isArray(value.accepts)) {
    throw new Error('The quote has no valid x402 v2 payment request');
  }
  const requirements = value.accepts[0];
  if (!isRecord(requirements)) throw new Error('The quote has no payment route');
  const network = requirements.network;
  const expectedNetwork =
    process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? SOLANA_DEVNET : SOLANA_MAINNET;
  const expectedAsset = expectedNetwork === SOLANA_MAINNET ? USDC_MAINNET : USDC_DEVNET;
  if (
    requirements.scheme !== 'exact' ||
    network !== expectedNetwork ||
    requirements.asset !== expectedAsset ||
    requirements.amount !== quoteAmount ||
    typeof requirements.payTo !== 'string' ||
    !isBase58Address(requirements.payTo) ||
    requirements.maxTimeoutSeconds !== 300
  ) {
    throw new Error('The payment request does not match the fixed quote');
  }
  return {
    amount: quoteAmount,
    asset: expectedAsset,
    network: expectedNetwork,
    payTo: requirements.payTo,
  };
}

export function selectPaymentRequirements(
  version: number,
  requirements: PaymentRequirements[],
  terms: PaymentTerms,
): PaymentRequirements {
  if (version !== 2) throw new Error('Only x402 v2 payments are supported');
  const matches = requirements.filter(
    (candidate) =>
      candidate.scheme === 'exact' &&
      candidate.network === terms.network &&
      candidate.asset === terms.asset &&
      candidate.amount === terms.amount &&
      candidate.payTo === terms.payTo &&
      candidate.maxTimeoutSeconds === 300 &&
      isBase58Address(candidate.extra?.feePayer),
  );
  if (matches.length !== 1) {
    throw new Error('The payment challenge does not match the accepted quote');
  }
  return matches[0]!;
}

function walletSigner(
  account: WalletAccount,
  feature: SolanaSignTransactionFeature['solana:signTransaction'],
  network: PaymentTerms['network'],
): TransactionSigner {
  const signerAddress = address(account.address);
  const chain: `solana:${string}` = network === SOLANA_MAINNET ? 'solana:mainnet' : 'solana:devnet';
  if (!account.chains.includes(chain)) {
    throw new Error(`The connected wallet does not expose ${chain}`);
  }
  const encoder = getTransactionEncoder();
  const decoder = getTransactionDecoder();

  return {
    address: signerAddress,
    async signTransactions(transactions): Promise<readonly SignatureDictionary[]> {
      const signed = await feature.signTransaction(
        ...transactions.map((transaction) => ({
          account,
          chain,
          transaction: new Uint8Array(encoder.encode(transaction)),
        })),
      );
      if (signed.length !== transactions.length) {
        throw new Error('The wallet returned an incomplete signature batch');
      }
      return signed.map((result, index) => {
        const original = transactions[index]!;
        const decoded = decoder.decode(result.signedTransaction);
        if (!equalBytes(original.messageBytes, decoded.messageBytes)) {
          throw new Error('The wallet changed the payment transaction');
        }
        const signature = decoded.signatures[signerAddress];
        if (!signature) throw new Error('The wallet did not sign the payment transaction');
        return { [signerAddress]: signature };
      });
    },
  };
}

function equalBytes(
  left: { readonly length: number; readonly [index: number]: number },
  right: { readonly length: number; readonly [index: number]: number },
): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isBase58Address(value: unknown): value is string {
  return typeof value === 'string' && /^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(value);
}
