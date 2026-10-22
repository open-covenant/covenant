// Refunds for paid jobs that fail. The merchant wallet that received the
// payment sends the same amount back to the payer's USDC account, with
// Covenant's fee payer paying the network fee (and the account's rent if the
// payer has since closed it).
import {
  address, appendTransactionMessageInstructions, createTransactionMessage, getBase64EncodedWireTransaction,
  getSignatureFromTransaction, pipe, setTransactionMessageFeePayerSigner, setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from '@solana/kit';
import {
  findAssociatedTokenPda, getCreateAssociatedTokenIdempotentInstruction, getTransferCheckedInstruction, TOKEN_PROGRAM_ADDRESS,
} from '@solana-program/token';
import { getSetComputeUnitLimitInstruction, getSetComputeUnitPriceInstruction } from '@solana-program/compute-budget';

const MEMO_PROGRAM = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');

/** The memo a refund carries, naming the quote it returns the payment for. */
export function refundMemo(quoteId) { return `covenant.x402.refund:${quoteId}`; }

/** A fully signed refund transaction and its signature. */
export async function buildRefund({ feePayer, merchant, mint, payer, amount, quoteId, blockhash, lastValidBlockHeight }) {
  const [source] = await findAssociatedTokenPda({ owner: merchant.address, mint, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  const [destination] = await findAssociatedTokenPda({ owner: payer, mint, tokenProgram: TOKEN_PROGRAM_ADDRESS });
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    m => setTransactionMessageFeePayerSigner(feePayer, m),
    m => setTransactionMessageLifetimeUsingBlockhash({ blockhash, lastValidBlockHeight: BigInt(lastValidBlockHeight) }, m),
    m => appendTransactionMessageInstructions([
      getSetComputeUnitLimitInstruction({ units: 60_000 }),
      getSetComputeUnitPriceInstruction({ microLamports: 1_000 }),
      getCreateAssociatedTokenIdempotentInstruction({ payer: feePayer, ata: destination, owner: payer, mint }),
      getTransferCheckedInstruction({ source, mint, destination, authority: merchant, amount, decimals: 6 }),
      { programAddress: MEMO_PROGRAM, accounts: [], data: new TextEncoder().encode(refundMemo(quoteId)) },
    ], m),
  );
  const signed = await signTransactionMessageWithSigners(message);
  return { transaction: getBase64EncodedWireTransaction(signed), signature: getSignatureFromTransaction(signed) };
}
