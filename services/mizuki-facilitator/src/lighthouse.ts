import {
  address,
  decompileTransactionMessage,
  getAddressEncoder,
  getCompiledTransactionMessageDecoder,
  getProgramDerivedAddress,
  getTransactionDecoder,
  isSignerRole,
  isWritableRole,
  type Transaction,
} from '@solana/kit';
import { webcrypto } from 'node:crypto';

const COMPUTE_BUDGET_PROGRAM = 'ComputeBudget111111111111111111111111111111';
const MEMO_PROGRAM = 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr';
export const LIGHTHOUSE_PROGRAM = 'L2TExMFKdjpN9kozasaurPirfHy9P8sbXoAN1qA3S95';
const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const TOKEN_2022_PROGRAM = 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb';
const ASSOCIATED_TOKEN_PROGRAM = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';

const IX_TRANSFER_CHECKED = 12;
const IX_SET_COMPUTE_UNIT_LIMIT = 2;
const IX_SET_COMPUTE_UNIT_PRICE = 3;
const MAX_LIGHTHOUSE_INSTRUCTIONS = 6;
const MAX_COMPUTE_UNIT_LIMIT = 1_400_000n;
const MAX_COMPUTE_UNIT_PRICE_MICROLAMPORTS = 100_000n;
const LIGHTHOUSE_MEMORY_IDS = 4;

export interface PaymentTerms {
  amount: string;
  asset: string;
  payTo: string;
  feePayer: string;
  memo?: string;
}

export type LighthouseVerification =
  | { ok: true; payer: string; destination: string; tokenProgram: string }
  | { ok: false; reason: string };

type Instruction = ReturnType<typeof decompileTransactionMessage>['instructions'][number];

function fail(reason: string): LighthouseVerification {
  return { ok: false, reason };
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

/**
 * Lighthouse writes its pre-transfer snapshots into memory accounts derived
 * from the payer, so those writes cannot move the payment or any other asset.
 */
async function lighthouseMemoryAddresses(payer: string): Promise<Set<string>> {
  const encoder = getAddressEncoder();
  const result = new Set<string>();
  for (let memoryId = 0; memoryId < LIGHTHOUSE_MEMORY_IDS; memoryId += 1) {
    try {
      const [memory] = await getProgramDerivedAddress({
        programAddress: address(LIGHTHOUSE_PROGRAM),
        seeds: [
          new TextEncoder().encode('memory'),
          encoder.encode(address(payer)),
          new Uint8Array([memoryId]),
        ],
      });
      result.add(memory);
    } catch {
      // An off-curve seed for one id does not invalidate the others.
    }
  }
  return result;
}

function computeBudgetValue(
  instruction: Instruction | undefined,
  discriminator: number,
  byteLength: number,
): bigint | undefined {
  if (!instruction || instruction.programAddress !== COMPUTE_BUDGET_PROGRAM) return undefined;
  const data = instruction.data;
  if (!data || data.length !== byteLength || data[0] !== discriminator) return undefined;
  if ((instruction.accounts ?? []).length > 0) return undefined;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  return byteLength === 5 ? BigInt(view.getUint32(1, true)) : view.getBigUint64(1, true);
}

async function verifyPayerSignature(payer: string, transaction: Transaction): Promise<boolean> {
  const signature = transaction.signatures[address(payer)];
  if (!signature || signature.every((byte) => byte === 0)) return false;
  try {
    const key = await webcrypto.subtle.importKey(
      'raw',
      new Uint8Array(getAddressEncoder().encode(address(payer))),
      { name: 'Ed25519' },
      false,
      ['verify'],
    );
    return await webcrypto.subtle.verify(
      { name: 'Ed25519' },
      key,
      new Uint8Array(signature),
      new Uint8Array(transaction.messageBytes),
    );
  } catch {
    return false;
  }
}

/**
 * Verify a payment whose wallet bracketed the transfer with Lighthouse guards.
 *
 * Phantom inserts guard instructions both before and after the TransferChecked
 * (see x402-foundation/x402#2097), so guards cannot be matched positionally.
 * The non-Lighthouse instructions must be exactly the four we quoted, in
 * order; guards may sit anywhere, bounded in count, and may not introduce a
 * signer or widen write access beyond the transfer's own accounts, the fee
 * payer, and the payer-derived Lighthouse memory.
 */
export async function verifyLighthouseTransaction(
  transactionBase64: string,
  terms: PaymentTerms,
): Promise<LighthouseVerification> {
  let transaction: Transaction;
  try {
    transaction = getTransactionDecoder().decode(Buffer.from(transactionBase64, 'base64'));
  } catch {
    return fail('transaction could not be decoded');
  }

  const compiled = getCompiledTransactionMessageDecoder().decode(transaction.messageBytes);
  if (compiled.version === 0 && (compiled.addressTableLookups?.length ?? 0) > 0) {
    return fail('address table lookups are not accepted');
  }

  const signers = (compiled.staticAccounts ?? [])
    .slice(0, compiled.header.numSignerAccounts)
    .map(String);
  if (signers.length !== 2) return fail('payment must carry exactly two signers');
  if (signers[0] !== terms.feePayer) return fail('fee payer must be the first signer');
  const payer = signers[1] as string;

  let message: ReturnType<typeof decompileTransactionMessage>;
  try {
    message = decompileTransactionMessage(compiled);
  } catch {
    return fail('transaction accounts could not be decompiled');
  }
  if (message.feePayer.address !== terms.feePayer) return fail('fee payer was changed');

  const instructions = message.instructions ?? [];
  const guards = instructions.filter((ix) => ix.programAddress === LIGHTHOUSE_PROGRAM);
  const core = instructions.filter((ix) => ix.programAddress !== LIGHTHOUSE_PROGRAM);
  if (core.length !== 4) return fail(`payment must carry four instructions, saw ${core.length}`);
  if (guards.length > MAX_LIGHTHOUSE_INSTRUCTIONS) {
    return fail(`too many wallet guard instructions: ${guards.length}`);
  }

  const [limitIx, priceIx, transferIx, memoIx] = core as [
    Instruction,
    Instruction,
    Instruction,
    Instruction,
  ];

  const limit = computeBudgetValue(limitIx, IX_SET_COMPUTE_UNIT_LIMIT, 5);
  if (limit === undefined || limit > MAX_COMPUTE_UNIT_LIMIT) {
    return fail('compute unit limit is missing or above the network maximum');
  }
  const price = computeBudgetValue(priceIx, IX_SET_COMPUTE_UNIT_PRICE, 9);
  if (price === undefined || price > MAX_COMPUTE_UNIT_PRICE_MICROLAMPORTS) {
    return fail('compute unit price is missing or above the accepted maximum');
  }

  if (![TOKEN_PROGRAM, TOKEN_2022_PROGRAM].includes(transferIx.programAddress)) {
    return fail('third instruction is not an SPL token transfer');
  }
  const data = transferIx.data;
  const accounts = transferIx.accounts ?? [];
  if (!data || data.length < 10 || data[0] !== IX_TRANSFER_CHECKED || accounts.length !== 4) {
    return fail('transfer instruction layout is not TransferChecked');
  }
  const amount = new DataView(data.buffer, data.byteOffset, data.byteLength).getBigUint64(1, true);
  const tokenProgram = transferIx.programAddress;
  const source = await associatedTokenAddress(payer, terms.asset, tokenProgram);
  const destination = await associatedTokenAddress(terms.payTo, terms.asset, tokenProgram);
  if (amount !== BigInt(terms.amount)) return fail('transfer amount does not match the quote');
  if (
    accounts[0]?.address !== source ||
    accounts[1]?.address !== terms.asset ||
    accounts[2]?.address !== destination ||
    accounts[3]?.address !== payer ||
    !isSignerRole(accounts[3].role)
  ) {
    return fail('transfer accounts do not match the quote');
  }

  if (memoIx.programAddress !== MEMO_PROGRAM) return fail('fourth instruction is not a memo');
  if (terms.memo !== undefined) {
    const actual = memoIx.data ? new TextDecoder().decode(memoIx.data) : '';
    if (actual !== terms.memo) return fail('memo does not match the quote');
  }

  const memory = await lighthouseMemoryAddresses(payer);
  const transferWritable = new Set([source, destination]);
  for (const instruction of instructions) {
    const isGuard = instruction.programAddress === LIGHTHOUSE_PROGRAM;
    for (const account of instruction.accounts ?? []) {
      if (isSignerRole(account.role) && !signers.includes(account.address)) {
        return fail('an instruction introduced a new signer');
      }
      if (!isWritableRole(account.role)) continue;
      if (transferWritable.has(account.address)) continue;
      if (isGuard && (memory.has(account.address) || account.address === terms.feePayer)) continue;
      return fail(`write access widened to ${account.address}`);
    }
  }

  if (!(await verifyPayerSignature(payer, transaction))) {
    return fail('payer signature is missing or does not match this transaction');
  }

  return { ok: true, payer, destination, tokenProgram };
}
