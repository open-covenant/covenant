import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  blockhash,
  compileTransaction,
  createTransactionMessage,
  generateKeyPairSigner,
  getBase58Decoder,
  getProgramDerivedAddress,
  getAddressEncoder,
  getTransactionEncoder,
  partiallySignTransaction,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  type Address,
  type Instruction,
} from '@solana/kit';
import { LIGHTHOUSE_PROGRAM, type PaymentTerms } from './lighthouse.js';

export const COMPUTE_BUDGET = address('ComputeBudget111111111111111111111111111111');
export const MEMO = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');
export const TOKEN = address('TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA');
export const ASSOCIATED_TOKEN = address('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL');
export const USDC = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
export const AMOUNT = 2_000_000n;
export const MEMO_TEXT = 'mizuki:test-quote';

export async function ata(owner: string, mint: string): Promise<Address> {
  const encoder = getAddressEncoder();
  const [derived] = await getProgramDerivedAddress({
    programAddress: ASSOCIATED_TOKEN,
    seeds: [encoder.encode(address(owner)), encoder.encode(TOKEN), encoder.encode(address(mint))],
  });
  return derived;
}

export function computeLimit(units = 100_000): Instruction {
  const data = new Uint8Array(5);
  data[0] = 2;
  new DataView(data.buffer).setUint32(1, units, true);
  return { programAddress: COMPUTE_BUDGET, data };
}

export function computePrice(micro = 1_000n): Instruction {
  const data = new Uint8Array(9);
  data[0] = 3;
  new DataView(data.buffer).setBigUint64(1, micro, true);
  return { programAddress: COMPUTE_BUDGET, data };
}

export function transferChecked(
  source: Address,
  destination: Address,
  payer: Address,
  amount = AMOUNT,
): Instruction {
  const data = new Uint8Array(10);
  data[0] = 12;
  new DataView(data.buffer).setBigUint64(1, amount, true);
  data[9] = 6;
  return {
    programAddress: TOKEN,
    accounts: [
      { address: source, role: AccountRole.WRITABLE },
      { address: address(USDC), role: AccountRole.READONLY },
      { address: destination, role: AccountRole.WRITABLE },
      { address: payer, role: AccountRole.READONLY_SIGNER },
    ],
    data,
  };
}

export function memo(text = MEMO_TEXT): Instruction {
  return { programAddress: MEMO, data: new TextEncoder().encode(text) };
}

export function guard(size: number, accounts: Instruction['accounts'] = []): Instruction {
  return {
    programAddress: address(LIGHTHOUSE_PROGRAM),
    accounts,
    data: new Uint8Array(size).fill(1),
  };
}

export async function buildPayment(
  instructions: (context: {
    source: Address;
    destination: Address;
    payer: Address;
    feePayer: Address;
  }) => Instruction[],
): Promise<{ transaction: string; terms: PaymentTerms; payer: Address; feePayer: Address }> {
  const payerSigner = await generateKeyPairSigner();
  const feePayerSigner = await generateKeyPairSigner();
  const recipient = await generateKeyPairSigner();
  const source = await ata(payerSigner.address, USDC);
  const destination = await ata(recipient.address, USDC);

  const message = instructions({
    source,
    destination,
    payer: payerSigner.address,
    feePayer: feePayerSigner.address,
  }).reduce(
    (carry, instruction) => appendTransactionMessageInstruction(instruction, carry),
    pipe(
      createTransactionMessage({ version: 0 }),
      (value) => setTransactionMessageFeePayer(feePayerSigner.address, value),
      (value) =>
        setTransactionMessageLifetimeUsingBlockhash(
          {
            blockhash: blockhash(getBase58Decoder().decode(new Uint8Array(32).fill(7))),
            lastValidBlockHeight: 1n,
          },
          value,
        ),
    ),
  );

  const signed = await partiallySignTransaction([payerSigner.keyPair], compileTransaction(message));
  return {
    transaction: Buffer.from(getTransactionEncoder().encode(signed)).toString('base64'),
    terms: {
      amount: String(AMOUNT),
      asset: USDC,
      payTo: recipient.address,
      feePayer: feePayerSigner.address,
      memo: MEMO_TEXT,
    },
    payer: payerSigner.address,
    feePayer: feePayerSigner.address,
  };
}
