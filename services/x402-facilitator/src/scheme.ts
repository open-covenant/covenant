import { getCompiledTransactionMessageDecoder, getTransactionDecoder } from '@solana/kit';
import { ExactSvmScheme } from '@x402/svm/exact/facilitator';
import { LIGHTHOUSE_PROGRAM, verifyLighthouseTransaction, type PaymentTerms } from './lighthouse.js';

/**
 * Fee caps for a facilitator that settles anyone's payments. The stock path
 * bounds the limit and the price separately; the wallet-guard path bounds
 * their product, which is what the fee payer actually pays.
 */
export interface FeeCaps {
  maxComputeUnits: number;
  maxPriorityFeeMicroLamports: number;
  maxGuardedPriorityFeeLamports: bigint;
  /** Every signature costs the fee payer 5,000 lamports, signed or not by us. */
  maxRequiredSignatures: number;
}

interface VerifyResult {
  response: { isValid: boolean; invalidReason?: string; payer?: string };
  verificationPath: 'static' | 'smartWallet' | null;
  matchedTransfer?: { destination: string; programId: string };
}

type VerifyFn = (payload: unknown, requirements: unknown) => Promise<VerifyResult>;

interface SimulatingSigner {
  simulateTransaction(transaction: string, network: string): Promise<void>;
}

/**
 * Rejections that mean "these instructions are not in the order I expected",
 * which is the only thing a wallet's guard instructions can cause. Mirrors
 * LAYOUT_RECOVERABLE_REASONS in @x402/svm.
 *
 * Everything absent from this set (a wrong amount, wrong recipient, wrong
 * mint, wrong memo, a fee payer transferring its own funds, or a failed
 * simulation) is a decision about the payment itself and must stand.
 */
const LAYOUT_REJECTIONS = new Set([
  'invalid_exact_svm_payload_transaction_instructions_length',
  'invalid_exact_svm_payload_no_transfer_instruction',
  'invalid_exact_svm_payload_unknown_fourth_instruction',
  'invalid_exact_svm_payload_unknown_fifth_instruction',
  'invalid_exact_svm_payload_unknown_sixth_instruction',
  'invalid_exact_svm_payload_unknown_optional_instruction',
  'invalid_exact_svm_payload_transaction_instructions_compute_limit_instruction',
  'invalid_exact_svm_payload_transaction_instructions_compute_price_instruction',
]);

/**
 * Accept payments whose wallet bracketed the transfer with its own guard
 * instructions.
 *
 * The published verifier matches instructions positionally, so it rejects
 * every Phantom payment: Phantom inserts Lighthouse guards both before and
 * after the TransferChecked (x402-foundation/x402#2097). This wraps its
 * verification and re-examines only the payments it rejected *on layout*,
 * against the same requirements.
 *
 * Two properties this must preserve, because settlement broadcasts with
 * skipPreflight and the fee payer pays for a failed transaction just the same:
 *
 *   1. Only layout rejections are reconsidered. Reconsidering every rejection
 *      would override the amount, recipient, memo and simulation checks.
 *   2. The payment is simulated before it is accepted. A layout rejection
 *      happens before the library's own simulation runs, so nothing else has
 *      established that this transaction can actually succeed.
 *
 * The wrap is on the instance's verification hook rather than a subclass
 * because `settle` calls that hook internally; wrapping `verify` alone would
 * leave settlement on the unpatched path.
 *
 * Remove once https://github.com/x402-foundation/x402/pull/3318 ships.
 */
export function createLighthouseTolerantScheme(
  signer: ConstructorParameters<typeof ExactSvmScheme>[0],
  settlementCache: ConstructorParameters<typeof ExactSvmScheme>[1],
  log: (event: Record<string, unknown>) => void = () => {},
  caps?: FeeCaps,
): ExactSvmScheme {
  const scheme = new ExactSvmScheme(
    signer,
    settlementCache,
    caps && {
      maxComputeUnits: caps.maxComputeUnits,
      maxPriorityFeeMicroLamports: caps.maxPriorityFeeMicroLamports,
      maxRequiredSignatures: caps.maxRequiredSignatures,
    },
  );
  const hook = scheme as unknown as { _verify: VerifyFn };
  const stockVerify = hook._verify.bind(scheme) as VerifyFn;
  const simulator = signer as unknown as SimulatingSigner;
  const guardCap = caps && { maxPriorityFeeLamports: caps.maxGuardedPriorityFeeLamports };

  hook._verify = async (payload, requirements) => {
    const stock = await stockVerify(payload, requirements);
    if (stock.response.isValid) {
      // The library accepts Lighthouse instructions after the transfer without
      // looking at their accounts, and a Lighthouse memory write charges rent
      // to whichever signer it names, the fee payer included. A payment that
      // carries any must pass the same account rules as a guarded payment.
      const transaction = (payload as { payload?: { transaction?: unknown } })?.payload?.transaction;
      const terms = paymentTerms(requirements);
      if (typeof transaction !== 'string' || !terms) return stock;
      if (!hasProgram(transaction, LIGHTHOUSE_PROGRAM)) return stock;
      const guarded = await verifyLighthouseTransaction(transaction, terms, guardCap);
      if (guarded.ok) return stock;
      log({ event: 'facilitator_verify_rejected', stockReason: 'accepted', guardReason: guarded.reason });
      return {
        response: { isValid: false, invalidReason: 'invalid_exact_svm_payload_wallet_guards', payer: stock.response.payer },
        verificationPath: null,
      };
    }

    const reason = stock.response.invalidReason;
    if (typeof reason !== 'string' || !LAYOUT_REJECTIONS.has(reason)) return stock;

    const transaction = (payload as { payload?: { transaction?: unknown } })?.payload?.transaction;
    if (typeof transaction !== 'string') return stock;

    const terms = paymentTerms(requirements);
    if (!terms) return stock;

    const guarded = await verifyLighthouseTransaction(transaction, terms, guardCap);
    if (!guarded.ok) {
      log({
        event: 'facilitator_verify_rejected',
        stockReason: reason,
        guardReason: guarded.reason,
      });
      return stock;
    }

    const network = (requirements as { network?: unknown })?.network;
    try {
      await simulator.simulateTransaction(transaction, String(network));
    } catch (error) {
      log({
        event: 'facilitator_verify_simulation_failed',
        stockReason: reason,
        message: error instanceof Error ? error.message : String(error),
      });
      return stock;
    }

    log({ event: 'facilitator_verify_wallet_guards', payer: guarded.payer });
    return {
      response: { isValid: true, payer: guarded.payer },
      verificationPath: 'static',
      matchedTransfer: { destination: guarded.destination, programId: guarded.tokenProgram },
    };
  };

  return scheme;
}

/** Whether any instruction calls `program`. An undecodable transaction counts as yes. */
export function hasProgram(transactionBase64: string, program: string): boolean {
  try {
    const transaction = getTransactionDecoder().decode(Buffer.from(transactionBase64, 'base64'));
    const message = getCompiledTransactionMessageDecoder().decode(transaction.messageBytes);
    return message.instructions.some(
      (instruction) => String(message.staticAccounts[instruction.programAddressIndex]) === program,
    );
  } catch {
    return true;
  }
}

function paymentTerms(requirements: unknown): PaymentTerms | undefined {
  if (typeof requirements !== 'object' || requirements === null) return undefined;
  const value = requirements as Record<string, unknown>;
  const extra = (value.extra ?? {}) as Record<string, unknown>;
  const { amount, asset, payTo } = value;
  const { feePayer, memo } = extra;
  if (
    typeof amount !== 'string' ||
    typeof asset !== 'string' ||
    typeof payTo !== 'string' ||
    typeof feePayer !== 'string'
  ) {
    return undefined;
  }
  return { amount, asset, payTo, feePayer, ...(typeof memo === 'string' ? { memo } : {}) };
}
