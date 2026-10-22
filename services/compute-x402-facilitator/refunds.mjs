// Refunds for paid jobs that fail. The merchant wallet that received the
// payment sends the same amount back to the account it came from, with
// Covenant's fee payer paying the network fee.
//
// Each refund also claims a one-byte marker account derived from the
// merchant and the payment, owned by the Memo program, which never changes
// it. Only the merchant's signature can claim it, and a claimed marker cannot
// be claimed again, so a second refund of the same payment fails on chain
// whatever the caller asks for. Sending lamports to the marker beforehand
// does not block the claim.
import {
  address, appendTransactionMessageInstructions, createAddressWithSeed, createTransactionMessage, getBase64EncodedWireTransaction,
  getSignatureFromTransaction, pipe, setTransactionMessageFeePayerSigner, setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from '@solana/kit';
import { getTransferCheckedInstruction } from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';
import { getAllocateWithSeedInstruction, getTransferSolInstruction } from '@solana-program/system';
import { createHash } from 'node:crypto';

export const MEMO_PROGRAM = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');

/** The memo a refund carries, naming the quote it returns the payment for. */
export function refundMemo(quoteId) { return `covenant.x402.refund:${quoteId}`; }

/** The seed of a payment's refund marker: 32 hex characters of its signature's SHA-256. */
export function markerSeed(paymentSignature) { return createHash('sha256').update(paymentSignature).digest('hex').slice(0, 32); }

/** The marker account a refund of this payment claims. */
export function refundMarker(merchant, paymentSignature) {
  return createAddressWithSeed({ baseAddress: merchant, programAddress: MEMO_PROGRAM, seed: markerSeed(paymentSignature) });
}

/**
 * A fully signed refund transaction and its signature. `destination` is the
 * token account the payment came from; it is never created here, so a
 * refund cannot be made to pay for someone's account.
 */
export async function buildRefund({ feePayer, merchant, mint, source, destination, amount, quoteId, paymentSignature, markerLamports, blockhash, lastValidBlockHeight }) {
  const marker = await refundMarker(merchant.address, paymentSignature);
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    m => setTransactionMessageFeePayerSigner(feePayer, m),
    m => setTransactionMessageLifetimeUsingBlockhash({ blockhash, lastValidBlockHeight: BigInt(lastValidBlockHeight) }, m),
    m => appendTransactionMessageInstructions([
      getSetComputeUnitLimitInstruction({ units: 60_000 }),
      getSetComputeUnitPriceInstruction({ microLamports: 1_000 }),
      getTransferSolInstruction({ source: feePayer, destination: marker, amount: BigInt(markerLamports) }),
      // Allocating with a seed also hands the account to the Memo program.
      getAllocateWithSeedInstruction({ newAccount: marker, baseAccount: merchant, base: merchant.address,
        seed: markerSeed(paymentSignature), space: 1n, programAddress: MEMO_PROGRAM }),
      getTransferCheckedInstruction({ source, mint, destination, authority: merchant, amount, decimals: 6 }),
      { programAddress: MEMO_PROGRAM, accounts: [], data: new TextEncoder().encode(refundMemo(quoteId)) },
    ], m),
  );
  const signed = await signTransactionMessageWithSigners(message);
  return { transaction: getBase64EncodedWireTransaction(signed), signature: getSignatureFromTransaction(signed), marker };
}
