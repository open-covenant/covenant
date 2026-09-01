import { ExactSvmScheme } from '@x402/svm/exact/facilitator';
import { verifyLighthouseTransaction, type PaymentTerms } from './lighthouse.js';

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
 * Everything absent from this set — a wrong amount, wrong recipient, wrong
 * mint, wrong memo, a fee payer transferring its own funds, or a failed
 * simulation — is a decision about the payment itself and must stand.
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
): ExactSvmScheme {
  const scheme = new ExactSvmScheme(signer, settlementCache);
  const hook = scheme as unknown as { _verify: VerifyFn };
  const stockVerify = hook._verify.bind(scheme) as VerifyFn;
  const simulator = signer as unknown as SimulatingSigner;

  hook._verify = async (payload, requirements) => {
    const stock = await stockVerify(payload, requirements);
    if (stock.response.isValid) return stock;

    const reason = stock.response.invalidReason;
    if (typeof reason !== 'string' || !LAYOUT_REJECTIONS.has(reason)) return stock;

    const transaction = (payload as { payload?: { transaction?: unknown } })?.payload?.transaction;
    if (typeof transaction !== 'string') return stock;

    const terms = paymentTerms(requirements);
    if (!terms) return stock;

    const guarded = await verifyLighthouseTransaction(transaction, terms);
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
