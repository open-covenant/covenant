'use client';

import {
  address,
  decompileTransactionMessage,
  getAddressEncoder,
  getBase64Decoder,
  getCompiledTransactionMessageDecoder,
  getProgramDerivedAddress,
  getTransactionDecoder,
  getTransactionEncoder,
  isSignerRole,
  isWritableRole,
  type SignatureBytes,
  type SignatureDictionary,
  type Transaction,
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
const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const TOKEN_2022_PROGRAM = 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb';
const ASSOCIATED_TOKEN_PROGRAM = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';
const COMPUTE_BUDGET_PROGRAM = 'ComputeBudget111111111111111111111111111111';
const MEMO_PROGRAM = 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr';
export const LIGHTHOUSE_PROGRAM = 'L2TExMFKdjpN9kozasaurPirfHy9P8sbXoAN1qA3S95';
const MAX_PAYMENT_ATOMIC = 10_000_000n;
const MAX_LIGHTHOUSE_INSTRUCTIONS = 2;

export type PaymentTerms = {
  amount: string;
  asset: string;
  network: typeof SOLANA_MAINNET | typeof SOLANA_DEVNET;
  payTo: string;
  feePayer: string;
  memo: string;
};

export type PaymentClientStage = 'wallet_opened' | 'wallet_signed' | 'submitting';

export type PaymentClientErrorCode =
  | 'challenge_invalid'
  | 'rpc_unavailable'
  | 'wallet_disconnected'
  | 'wallet_rejected'
  | 'wallet_response_invalid'
  | 'wallet_signature_invalid'
  | 'wallet_transaction_unsafe';

export class PaymentClientError extends Error {
  constructor(
    readonly code: PaymentClientErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = 'PaymentClientError';
  }
}

export function createPaymentFetch(input: {
  account: WalletAccount;
  feature: SolanaSignTransactionFeature['solana:signTransaction'];
  quotePayment: unknown;
  quoteAmount: string;
  onStage?: (stage: PaymentClientStage) => void | Promise<void>;
  request?: typeof fetch;
}): typeof fetch {
  const terms = parsePaymentTerms(input.quotePayment, input.quoteAmount);
  let walletTransaction: Uint8Array | undefined;
  const signer = walletSigner(input.account, input.feature, terms.network, terms, {
    onStage: input.onStage,
    capture: (transaction) => {
      walletTransaction = transaction;
    },
  });
  const rpcUrl = process.env.NEXT_PUBLIC_SOLANA_RPC_URL;
  const delegate = new ExactSvmScheme(signer, rpcUrl ? { rpcUrl } : undefined);
  const scheme = {
    scheme: delegate.scheme,
    findDefaultAsset: delegate.findDefaultAsset,
    async createPaymentPayload(
      ...args: Parameters<ExactSvmScheme['createPaymentPayload']>
    ): Promise<Awaited<ReturnType<ExactSvmScheme['createPaymentPayload']>>> {
      walletTransaction = undefined;
      const payload = await delegate.createPaymentPayload(...args);
      if (!walletTransaction) {
        throw new PaymentClientError(
          'wallet_response_invalid',
          'The wallet did not return a signed payment transaction',
        );
      }
      return {
        ...payload,
        payload: {
          ...payload.payload,
          transaction: getBase64Decoder().decode(walletTransaction),
        },
      };
    },
  };
  const request = input.request ?? fetch;
  const observedRequest: typeof fetch = async (target, init) => {
    const outgoing = new Request(target, init);
    if (outgoing.headers.has('payment-signature') || outgoing.headers.has('x-payment')) {
      await input.onStage?.('submitting');
    }
    return request(target, init);
  };

  return wrapFetchWithPaymentFromConfig(observedRequest, {
    schemes: [{ network: terms.network, client: scheme }],
    spendControls: {
      allowedAssets: [
        {
          network: terms.network,
          asset: terms.asset,
          maxAmountPerPayment: terms.amount,
        },
      ],
    },
    paymentRequirementsSelector: (version, requirements) =>
      selectPaymentRequirements(version, requirements, terms),
  });
}

export function paymentPreparationError(cause: unknown, quoteAmount: string): string {
  const message = cause instanceof Error ? cause.message : '';
  if (cause instanceof PaymentClientError) {
    switch (cause.code) {
      case 'wallet_rejected':
        return 'Payment was cancelled in your wallet. Your wallet was not charged and no job was created.';
      case 'wallet_disconnected':
        return 'The payment wallet disconnected. Reconnect it and try again. Your wallet was not charged and no job was created.';
      case 'wallet_signature_invalid':
      case 'wallet_response_invalid':
      case 'wallet_transaction_unsafe':
        return 'The wallet could not safely authorize this payment. Update or reconnect the wallet, or choose another supported wallet. No payment or job was created.';
      case 'challenge_invalid':
        return 'The payment request no longer matches this quote. Refresh the page and request a new quote. No payment or job was created.';
      case 'rpc_unavailable':
        return 'The Solana payment network could not prepare the transaction. Try again in a moment. Your wallet was not charged and no job was created.';
    }
  }

  if (/insufficient|not enough|low balance|balance.*(?:small|low|zero)/i.test(message)) {
    return `Your connected wallet does not have enough USDC on Solana to pay the ${quoteAmount} quote. Add USDC to this wallet and try again. No payment or job was created.`;
  }
  if (/rejected by spendControls|per.payment.cap|maxAmountPerPayment/i.test(message)) {
    return 'Workbench could not authorize this quote amount. Refresh the page and request a new quote. Your wallet was not charged and no job was created.';
  }
  if (/user rejected|declined|cancelled|canceled/i.test(message)) {
    return 'Payment was cancelled in your wallet. Your wallet was not charged and no job was created.';
  }
  if (/does not expose solana:mainnet/i.test(message)) {
    return 'The connected wallet is not available on Solana mainnet. Switch networks or connect another Solana wallet. No payment or job was created.';
  }
  if (/payment (?:request|challenge) does not match|fixed quote|memo/i.test(message)) {
    return 'The payment request no longer matches this quote. Refresh the page and request a new quote. No payment or job was created.';
  }
  if (
    /rpc|failed to fetch|network|http[^\n]{0,30}(?:4\d\d|5\d\d)|blockhash|account info/i.test(
      message,
    )
  ) {
    return 'The Solana payment network could not prepare the transaction. Try again in a moment. Your wallet was not charged and no job was created.';
  }

  return 'Payment could not start. Reconnect the wallet and try again. Your wallet was not charged and no job was created.';
}

export function parsePaymentTerms(value: unknown, quoteAmount: string): PaymentTerms {
  if (!isRecord(value) || value.x402Version !== 2 || !Array.isArray(value.accepts)) {
    throw new PaymentClientError('challenge_invalid', 'The quote has no valid x402 v2 payment request');
  }
  if (!/^\d+$/.test(quoteAmount) || BigInt(quoteAmount) < 1n || BigInt(quoteAmount) > MAX_PAYMENT_ATOMIC) {
    throw new PaymentClientError('challenge_invalid', 'The fixed quote exceeds the payment limit');
  }
  const requirements = value.accepts[0];
  if (!isRecord(requirements)) {
    throw new PaymentClientError('challenge_invalid', 'The quote has no payment route');
  }
  const network = requirements.network;
  const expectedNetwork =
    process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? SOLANA_DEVNET : SOLANA_MAINNET;
  const expectedAsset = expectedNetwork === SOLANA_MAINNET ? USDC_MAINNET : USDC_DEVNET;
  const extra = isRecord(requirements.extra) ? requirements.extra : undefined;
  if (
    requirements.scheme !== 'exact' ||
    network !== expectedNetwork ||
    requirements.asset !== expectedAsset ||
    requirements.amount !== quoteAmount ||
    typeof requirements.payTo !== 'string' ||
    !isBase58Address(requirements.payTo) ||
    requirements.maxTimeoutSeconds !== 300 ||
    !isBase58Address(extra?.feePayer) ||
    typeof extra.memo !== 'string' ||
    !validPaymentMemo(extra.memo)
  ) {
    throw new PaymentClientError(
      'challenge_invalid',
      'The payment request does not match the fixed quote',
    );
  }
  return {
    amount: quoteAmount,
    asset: expectedAsset,
    network: expectedNetwork,
    payTo: requirements.payTo,
    feePayer: extra.feePayer,
    memo: extra.memo,
  };
}

export function selectPaymentRequirements(
  version: number,
  requirements: PaymentRequirements[],
  terms: PaymentTerms,
): PaymentRequirements {
  if (version !== 2) {
    throw new PaymentClientError('challenge_invalid', 'Only x402 v2 payments are supported');
  }
  const matches = requirements.filter((candidate) => {
    const extra = isRecord(candidate.extra) ? candidate.extra : undefined;
    return (
      candidate.scheme === 'exact' &&
      candidate.network === terms.network &&
      candidate.asset === terms.asset &&
      candidate.amount === terms.amount &&
      candidate.payTo === terms.payTo &&
      candidate.maxTimeoutSeconds === 300 &&
      extra?.feePayer === terms.feePayer &&
      extra?.memo === terms.memo
    );
  });
  if (matches.length !== 1) {
    throw new PaymentClientError(
      'challenge_invalid',
      'The payment challenge does not match the accepted quote',
    );
  }
  return matches[0]!;
}

export async function validateWalletSignedTransaction(
  originalBytes: Uint8Array,
  signedBytes: Uint8Array,
  input: {
    payer: WalletAccount;
    terms: PaymentTerms;
    verifySignature?: (
      publicKey: Uint8Array,
      signature: SignatureBytes,
      message: Uint8Array,
    ) => Promise<boolean>;
  },
): Promise<Transaction> {
  let original: Transaction;
  let signed: Transaction;
  try {
    const decoder = getTransactionDecoder();
    original = decoder.decode(originalBytes);
    signed = decoder.decode(signedBytes);
  } catch (cause) {
    throw new PaymentClientError(
      'wallet_response_invalid',
      'The wallet returned an invalid payment transaction',
      { cause },
    );
  }

  const messageDecoder = getCompiledTransactionMessageDecoder();
  const originalCompiled = messageDecoder.decode(original.messageBytes);
  const signedCompiled = messageDecoder.decode(signed.messageBytes);
  const originalSigners = signerAddresses(originalCompiled);
  const signedSigners = signerAddresses(signedCompiled);
  if (
    String(originalCompiled.lifetimeToken) !== String(signedCompiled.lifetimeToken) ||
    originalSigners[0] !== input.terms.feePayer ||
    !sameStrings(originalSigners, signedSigners) ||
    !signedSigners.includes(input.payer.address) ||
    hasAddressTableLookups(originalCompiled) ||
    hasAddressTableLookups(signedCompiled)
  ) {
    throw unsafeWalletTransaction();
  }

  let originalMessage: ReturnType<typeof decompileTransactionMessage>;
  let signedMessage: ReturnType<typeof decompileTransactionMessage>;
  try {
    originalMessage = decompileTransactionMessage(originalCompiled);
    signedMessage = decompileTransactionMessage(signedCompiled);
  } catch (cause) {
    throw new PaymentClientError(
      'wallet_transaction_unsafe',
      'The wallet added unsupported transaction accounts',
      { cause },
    );
  }
  const originalInstructions = originalMessage.instructions;
  const signedInstructions = signedMessage.instructions;
  if (
    originalMessage.feePayer.address !== input.terms.feePayer ||
    signedMessage.feePayer.address !== input.terms.feePayer ||
    originalInstructions.length !== 4 ||
    signedInstructions.length < 4 ||
    signedInstructions.length > 4 + MAX_LIGHTHOUSE_INSTRUCTIONS
  ) {
    throw unsafeWalletTransaction();
  }

  if (
    originalInstructions[0]?.programAddress !== COMPUTE_BUDGET_PROGRAM ||
    originalInstructions[1]?.programAddress !== COMPUTE_BUDGET_PROGRAM ||
    !equalInstruction(originalInstructions[0], signedInstructions[0]) ||
    !equalInstruction(originalInstructions[1], signedInstructions[1]) ||
    !equalInstruction(originalInstructions[2], signedInstructions[2])
  ) {
    throw unsafeWalletTransaction();
  }

  await validateTransfer(originalInstructions[2], input.payer.address, input.terms);
  validateOptionalInstructions(originalInstructions[3], signedInstructions.slice(3), input.terms);
  validateWritableAccounts(originalInstructions, signedInstructions);

  const signature = signed.signatures[address(input.payer.address)];
  if (!signature || signature.every((byte) => byte === 0)) {
    throw new PaymentClientError(
      'wallet_signature_invalid',
      'The wallet did not sign the payment transaction',
    );
  }
  const verify = input.verifySignature ?? verifyTransactionSignature;
  if (
    !(await verify(
      new Uint8Array(input.payer.publicKey),
      signature,
      new Uint8Array(signed.messageBytes),
    ))
  ) {
    throw new PaymentClientError(
      'wallet_signature_invalid',
      'The wallet returned a signature for a different transaction',
    );
  }

  return signed;
}

function walletSigner(
  account: WalletAccount,
  feature: SolanaSignTransactionFeature['solana:signTransaction'],
  network: PaymentTerms['network'],
  terms: PaymentTerms,
  hooks: {
    onStage?: (stage: PaymentClientStage) => void | Promise<void>;
    capture: (transaction: Uint8Array) => void;
  },
): TransactionSigner {
  const signerAddress = address(account.address);
  const chain: `solana:${string}` = network === SOLANA_MAINNET ? 'solana:mainnet' : 'solana:devnet';
  if (!account.chains.includes(chain)) {
    throw new PaymentClientError(
      'wallet_disconnected',
      `The connected wallet does not expose ${chain}`,
    );
  }
  const encoder = getTransactionEncoder();

  return {
    address: signerAddress,
    async signTransactions(transactions): Promise<readonly SignatureDictionary[]> {
      await hooks.onStage?.('wallet_opened');
      let walletResults: Awaited<ReturnType<typeof feature.signTransaction>>;
      try {
        walletResults = await feature.signTransaction(
          ...transactions.map((transaction) => ({
            account,
            chain,
            transaction: new Uint8Array(encoder.encode(transaction)),
          })),
        );
      } catch (cause) {
        const message = cause instanceof Error ? cause.message : '';
        const code = /reject|declin|cancel/i.test(message) ? 'wallet_rejected' : 'wallet_disconnected';
        throw new PaymentClientError(code, 'The wallet did not authorize the payment', { cause });
      }
      if (walletResults.length !== transactions.length) {
        throw new PaymentClientError(
          'wallet_response_invalid',
          'The wallet returned an incomplete signature batch',
        );
      }

      const signatures: SignatureDictionary[] = [];
      for (let index = 0; index < walletResults.length; index += 1) {
        const original = transactions[index]!;
        const originalBytes = new Uint8Array(encoder.encode(original));
        const signedBytes = new Uint8Array(walletResults[index]!.signedTransaction);
        const signed = await validateWalletSignedTransaction(originalBytes, signedBytes, {
          payer: account,
          terms,
        });
        const signature = signed.signatures[signerAddress];
        if (!signature) {
          throw new PaymentClientError(
            'wallet_signature_invalid',
            'The wallet did not sign the payment transaction',
          );
        }
        hooks.capture(signedBytes);
        signatures.push({ [signerAddress]: signature });
      }
      await hooks.onStage?.('wallet_signed');
      return signatures;
    },
  };
}

async function validateTransfer(
  instruction: ReturnType<typeof decompileTransactionMessage>['instructions'][number] | undefined,
  payer: string,
  terms: PaymentTerms,
): Promise<void> {
  if (!instruction || ![TOKEN_PROGRAM, TOKEN_2022_PROGRAM].includes(instruction.programAddress)) {
    throw unsafeWalletTransaction();
  }
  const data = instruction.data;
  const accounts = instruction.accounts ?? [];
  if (!data || data.length < 10 || data[0] !== 12 || accounts.length !== 4) {
    throw unsafeWalletTransaction();
  }
  const amount = new DataView(data.buffer, data.byteOffset, data.byteLength).getBigUint64(1, true);
  const tokenProgram = instruction.programAddress;
  const source = await associatedTokenAddress(payer, terms.asset, tokenProgram);
  const destination = await associatedTokenAddress(terms.payTo, terms.asset, tokenProgram);
  if (
    amount !== BigInt(terms.amount) ||
    accounts[0]?.address !== source ||
    accounts[1]?.address !== terms.asset ||
    accounts[2]?.address !== destination ||
    accounts[3]?.address !== payer ||
    !isSignerRole(accounts[3].role)
  ) {
    throw unsafeWalletTransaction();
  }
}

function validateOptionalInstructions(
  originalMemo: ReturnType<typeof decompileTransactionMessage>['instructions'][number] | undefined,
  instructions: readonly ReturnType<typeof decompileTransactionMessage>['instructions'][number][],
  terms: PaymentTerms,
): void {
  if (
    !originalMemo ||
    originalMemo.programAddress !== MEMO_PROGRAM ||
    new TextDecoder().decode(originalMemo.data) !== terms.memo
  ) {
    throw unsafeWalletTransaction();
  }
  const memos = instructions.filter((instruction) => instruction.programAddress === MEMO_PROGRAM);
  const lighthouse = instructions.filter(
    (instruction) => instruction.programAddress === LIGHTHOUSE_PROGRAM,
  );
  if (
    memos.length !== 1 ||
    !equalInstruction(originalMemo, memos[0]) ||
    lighthouse.length > MAX_LIGHTHOUSE_INSTRUCTIONS ||
    memos.length + lighthouse.length !== instructions.length
  ) {
    throw unsafeWalletTransaction();
  }
  for (const instruction of lighthouse) {
    if ((instruction.accounts ?? []).some((account) => isSignerRole(account.role))) {
      throw unsafeWalletTransaction();
    }
  }
}

function validateWritableAccounts(
  original: ReturnType<typeof decompileTransactionMessage>['instructions'],
  signed: ReturnType<typeof decompileTransactionMessage>['instructions'],
): void {
  const originalWritable = writableAddresses(original);
  for (const instruction of signed) {
    for (const account of instruction.accounts ?? []) {
      if (isWritableRole(account.role) && !originalWritable.has(account.address)) {
        throw unsafeWalletTransaction();
      }
    }
  }
}

function writableAddresses(
  instructions: ReturnType<typeof decompileTransactionMessage>['instructions'],
): Set<string> {
  const result = new Set<string>();
  for (const instruction of instructions) {
    for (const account of instruction.accounts ?? []) {
      if (isWritableRole(account.role)) result.add(account.address);
    }
  }
  return result;
}

async function associatedTokenAddress(
  owner: string,
  mint: string,
  tokenProgram: string,
): Promise<string> {
  const encoder = getAddressEncoder();
  const [derived] = await getProgramDerivedAddress({
    programAddress: address(ASSOCIATED_TOKEN_PROGRAM),
    seeds: [
      encoder.encode(address(owner)),
      encoder.encode(address(tokenProgram)),
      encoder.encode(address(mint)),
    ],
  });
  return derived;
}

function signerAddresses(
  message: ReturnType<ReturnType<typeof getCompiledTransactionMessageDecoder>['decode']>,
): string[] {
  return message.staticAccounts
    .slice(0, message.header.numSignerAccounts)
    .map((account) => String(account));
}

function hasAddressTableLookups(
  message: ReturnType<ReturnType<typeof getCompiledTransactionMessageDecoder>['decode']>,
): boolean {
  return message.version === 0 && Boolean(message.addressTableLookups?.length);
}

function equalInstruction(
  left: ReturnType<typeof decompileTransactionMessage>['instructions'][number] | undefined,
  right: ReturnType<typeof decompileTransactionMessage>['instructions'][number] | undefined,
): boolean {
  if (!left || !right || left.programAddress !== right.programAddress) return false;
  if (!equalBytes(left.data ?? new Uint8Array(), right.data ?? new Uint8Array())) return false;
  const leftAccounts = left.accounts ?? [];
  const rightAccounts = right.accounts ?? [];
  if (leftAccounts.length !== rightAccounts.length) return false;
  return leftAccounts.every(
    (account, index) =>
      account.address === rightAccounts[index]?.address && account.role === rightAccounts[index]?.role,
  );
}

async function verifyTransactionSignature(
  publicKey: Uint8Array,
  signature: SignatureBytes,
  message: Uint8Array,
): Promise<boolean> {
  try {
    const key = await crypto.subtle.importKey(
      'raw',
      new Uint8Array(publicKey),
      { name: 'Ed25519' },
      false,
      ['verify'],
    );
    return crypto.subtle.verify(
      { name: 'Ed25519' },
      key,
      new Uint8Array(signature),
      new Uint8Array(message),
    );
  } catch {
    return false;
  }
}

function unsafeWalletTransaction(): PaymentClientError {
  return new PaymentClientError(
    'wallet_transaction_unsafe',
    'The wallet changed protected payment instructions',
  );
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

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function validPaymentMemo(value: string): boolean {
  const bytes = new TextEncoder().encode(value);
  return value.length > 0 && bytes.byteLength <= 256 && /^mizuki:[A-Za-z0-9:_-]+$/.test(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isBase58Address(value: unknown): value is string {
  return typeof value === 'string' && /^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(value);
}
