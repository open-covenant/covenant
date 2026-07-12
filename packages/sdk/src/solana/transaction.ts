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
// it was built with. Failures throw with the signature and, where the RPC has
// them, the program logs, so a caller can actually debug a rejected write.
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
  const commitment = options.commitment ?? 'confirmed';

  let signature: string;
  try {
    signature = await connection.sendRawTransaction(signed.serialize(), {
      skipPreflight: options.skipPreflight ?? false,
      // Leave maxRetries unset by default so the RPC rebroadcasts until the
      // blockhash expires; a low cap drops the tx under congestion.
      ...(options.maxRetries !== undefined ? { maxRetries: options.maxRetries } : {}),
    });
  } catch (err) {
    throw await withLogs(connection, err, 'transaction send failed');
  }

  let confirmed;
  try {
    confirmed = await connection.confirmTransaction({ signature, blockhash, lastValidBlockHeight }, commitment);
  } catch (err) {
    // The blockhash-confirmation strategy relies on a WebSocket subscription; an
    // HTTP-only or WS-blocked provider can report a landed tx as expired. Poll
    // the signature status for ground truth before deciding it failed.
    const status = (await connection.getSignatureStatuses([signature]).catch(() => null))?.value?.[0];
    if (status && !status.err) return signature;
    if (status?.err) throw await failedTransaction(connection, signature, status.err);
    throw new Error(
      `could not confirm transaction ${signature}: ${(err as Error).message}. It may still land; re-check the signature.`,
    );
  }

  if (confirmed.value.err) {
    throw await failedTransaction(connection, signature, confirmed.value.err);
  }
  return signature;
}

// SendTransactionError carries preflight logs; surface them instead of a bare message.
async function withLogs(connection: Connection, err: unknown, prefix: string): Promise<Error> {
  const e = err as { logs?: string[]; getLogs?: (c: Connection) => Promise<string[]>; message?: string };
  let logs = e.logs;
  if (!logs && typeof e.getLogs === 'function') {
    logs = await e.getLogs(connection).catch(() => undefined);
  }
  const message = e.message ?? String(err);
  return new Error(logs?.length ? `${prefix}: ${message}\n${logs.join('\n')}` : `${prefix}: ${message}`);
}

// confirmTransaction returns only the error code; pull the execution logs so a
// failed instruction is debuggable.
async function failedTransaction(connection: Connection, signature: string, err: unknown): Promise<Error> {
  const tx = await connection
    .getTransaction(signature, { maxSupportedTransactionVersion: 0 })
    .catch(() => null);
  const logs = tx?.meta?.logMessages;
  const base = `transaction ${signature} failed: ${JSON.stringify(err)}`;
  return new Error(logs?.length ? `${base}\n${logs.join('\n')}` : base);
}
