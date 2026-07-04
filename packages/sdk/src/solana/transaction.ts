import {
  ComputeBudgetProgram,
  type Commitment,
  type Connection,
  PublicKey,
  Transaction,
  type TransactionInstruction,
} from '@solana/web3.js';

export interface BuildTransactionOptions {
  // Priority fee: a compute-unit price in micro-lamports, and optionally a limit.
  computeUnitPriceMicroLamports?: number;
  computeUnitLimit?: number;
  commitment?: Commitment;
}

// Assemble a legacy transaction: optional compute-budget instructions, the
// program instructions, fee payer, and a fresh blockhash ready for signing.
export async function buildTransaction(
  connection: Connection,
  feePayer: PublicKey,
  instructions: TransactionInstruction[],
  options: BuildTransactionOptions = {},
): Promise<Transaction> {
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash(
    options.commitment ?? 'confirmed',
  );
  const tx = new Transaction();
  if (options.computeUnitLimit !== undefined) {
    tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: options.computeUnitLimit }));
  }
  if (options.computeUnitPriceMicroLamports !== undefined) {
    tx.add(
      ComputeBudgetProgram.setComputeUnitPrice({ microLamports: options.computeUnitPriceMicroLamports }),
    );
  }
  tx.add(...instructions);
  tx.feePayer = feePayer;
  tx.recentBlockhash = blockhash;
  tx.lastValidBlockHeight = lastValidBlockHeight;
  return tx;
}

export interface SendOptions {
  commitment?: Commitment;
  skipPreflight?: boolean;
  maxRetries?: number;
}

// Send an already-signed legacy transaction and confirm it against the blockhash
// it was built with. Throws with the signature if the transaction lands in error.
export async function sendAndConfirmSignedTransaction(
  connection: Connection,
  signed: Transaction,
  options: SendOptions = {},
): Promise<string> {
  const blockhash = signed.recentBlockhash;
  const lastValidBlockHeight = signed.lastValidBlockHeight;
  if (!blockhash || lastValidBlockHeight === undefined) {
    throw new Error('transaction is missing a recent blockhash; build it with buildTransaction first');
  }
  const signature = await connection.sendRawTransaction(signed.serialize(), {
    skipPreflight: options.skipPreflight ?? false,
    maxRetries: options.maxRetries ?? 3,
  });
  const confirmation = await connection.confirmTransaction(
    { signature, blockhash, lastValidBlockHeight },
    options.commitment ?? 'confirmed',
  );
  if (confirmation.value.err) {
    throw new Error(`transaction ${signature} failed: ${JSON.stringify(confirmation.value.err)}`);
  }
  return signature;
}
