import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  blockhash,
  compileTransaction,
  createTransactionMessage,
  decompileTransactionMessage,
  getBase58Decoder,
  getCompiledTransactionMessageDecoder,
  getProgramDerivedAddress,
  getAddressEncoder,
  getTransactionDecoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  type SignatureBytes,
} from '@solana/kit';
import type { WalletAccount } from '@wallet-standard/base';
import { afterEach, describe, expect, it } from 'vitest';
import {
  LIGHTHOUSE_PROGRAM,
  parsePaymentTerms,
  paymentPreparationError,
  selectPaymentRequirements,
  validateWalletSignedTransaction,
  type PaymentTerms,
} from './x402';

const network = 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp';
const asset = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const payTo = '2'.repeat(32);
const feePayer = '3'.repeat(32);
const memo = 'mizuki:quote:11111111-1111-4111-8111-111111111111';
const requirements = {
  scheme: 'exact',
  network,
  asset,
  amount: '2000000',
  payTo,
  maxTimeoutSeconds: 300,
  extra: { feePayer, memo },
} as const;

afterEach(() => {
  delete process.env.NEXT_PUBLIC_SOLANA_NETWORK;
});

describe('x402 quote policy', () => {
  it('selects the one route that exactly matches the accepted quote', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(selectPaymentRequirements(2, [requirements], terms)).toEqual(requirements);
  });

  it.each([
    ['amount', { ...requirements, amount: '10000000' }],
    ['asset', { ...requirements, asset: '5'.repeat(32) }],
    ['network', { ...requirements, network: 'solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1' }],
  ])('rejects a changed %s before the wallet signs', (_field, changed) => {
    expect(() =>
      parsePaymentTerms({ x402Version: 2, accepts: [changed] }, requirements.amount),
    ).toThrow('does not match');
  });

  it('rejects a recipient changed after quote acceptance', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(() =>
      selectPaymentRequirements(2, [{ ...requirements, payTo: '4'.repeat(32) }], terms),
    ).toThrow('does not match');
  });

  it('requires the deterministic quote memo on both the quote and live challenge', () => {
    expect(() =>
      parsePaymentTerms(
        { x402Version: 2, accepts: [{ ...requirements, extra: { feePayer } }] },
        requirements.amount,
      ),
    ).toThrow('does not match');

    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(() =>
      selectPaymentRequirements(
        2,
        [{ ...requirements, extra: { feePayer, memo: `${memo}-changed` } }],
        terms,
      ),
    ).toThrow('does not match');
  });

  it('enforces the ten USDC product ceiling before opening a wallet', () => {
    expect(() =>
      parsePaymentTerms(
        { x402Version: 2, accepts: [{ ...requirements, amount: '10000001' }] },
        '10000001',
      ),
    ).toThrow('payment limit');
  });

  it('rejects ambiguous matching routes', () => {
    const terms = parsePaymentTerms(
      { x402Version: 2, accepts: [requirements] },
      requirements.amount,
    );
    expect(() => selectPaymentRequirements(2, [requirements, requirements], terms)).toThrow(
      'does not match',
    );
  });

  it('explains an unfunded wallet without exposing SDK internals', () => {
    expect(paymentPreparationError(new Error('insufficient token balance'), '2 USDC')).toBe(
      'Your connected wallet does not have enough USDC on Solana to pay the 2 USDC quote. Add USDC to this wallet and try again. No payment or job was created.',
    );
  });

  it('does not expose spend-control configuration to customers', () => {
    const message = paymentPreparationError(
      new Error('All payment requirements were rejected by spendControls.maxAmountPerPayment'),
      '2 USDC',
    );
    expect(message).toContain('Workbench could not authorize this quote amount');
    expect(message).not.toContain('spendControls');
    expect(message).not.toContain('maxAmountPerPayment');
  });

  it.each([400, 403])(
    'reports an unavailable browser RPC after HTTP %s without blaming the wallet balance',
    (status) => {
      const message = paymentPreparationError(
        new Error(`Failed to create payment payload: HTTP error (${status}): RPC unavailable`),
        '2 USDC',
      );

      expect(message).toContain('Solana payment network could not prepare the transaction');
      expect(message).not.toContain('balance');
      expect(message).not.toContain(String(status));
    },
  );

  it('does not claim an unknown preparation error means insufficient funds', () => {
    const message = paymentPreparationError(new Error('unexpected wallet adapter error'), '2 USDC');

    expect(message).toContain('Reconnect the wallet');
    expect(message).not.toContain('at least 2 USDC');
  });
});

describe('wallet-returned SVM transaction validation', () => {
  it('preserves an unchanged transaction and Phantom or Solflare Lighthouse additions', async () => {
    const fixture = await paymentFixture();

    await expect(validate(fixture, fixture.original)).resolves.toBeDefined();
    await expect(validate(fixture, addLighthouse(fixture.original, 1))).resolves.toBeDefined();
    await expect(validate(fixture, addLighthouse(fixture.original, 2))).resolves.toBeDefined();
  });

  it('rejects a changed transfer, memo, unknown program, signer, or writable account', async () => {
    const fixture = await paymentFixture();

    await expect(validate(fixture, changeTransferAmount(fixture.original))).rejects.toMatchObject({
      code: 'wallet_transaction_unsafe',
    });
    await expect(validate(fixture, changeMemo(fixture.original))).rejects.toMatchObject({
      code: 'wallet_transaction_unsafe',
    });
    await expect(
      validate(
        fixture,
        appendInstruction(fixture.original, 'BPFLoaderUpgradeab1e11111111111111111111111'),
      ),
    ).rejects.toMatchObject({ code: 'wallet_transaction_unsafe' });
    await expect(
      validate(fixture, addLighthouse(fixture.original, 1, AccountRole.READONLY_SIGNER)),
    ).rejects.toMatchObject({ code: 'wallet_transaction_unsafe' });
    await expect(
      validate(fixture, addLighthouse(fixture.original, 1, AccountRole.WRITABLE)),
    ).rejects.toMatchObject({ code: 'wallet_transaction_unsafe' });
  });

  it('rejects an absent or invalid payer signature', async () => {
    const fixture = await paymentFixture();
    const unsigned = transactionWithSignature(fixture.original, fixture.payer.address, 0);
    await expect(validate(fixture, unsigned)).rejects.toMatchObject({
      code: 'wallet_signature_invalid',
    });

    await expect(
      validateWalletSignedTransaction(fixture.original, fixture.original, {
        payer: fixture.payer,
        terms: fixture.terms,
        verifySignature: async () => false,
      }),
    ).rejects.toMatchObject({ code: 'wallet_signature_invalid' });
  });
});

const tokenProgram = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const associatedTokenProgram = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';
const computeBudgetProgram = 'ComputeBudget111111111111111111111111111111';
const memoProgram = 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr';

async function paymentFixture() {
  const payerAddress = getBase58Decoder().decode(new Uint8Array(32).fill(1));
  const feePayerAddress = getBase58Decoder().decode(new Uint8Array(32).fill(2));
  const recipient = getBase58Decoder().decode(new Uint8Array(32).fill(3));
  const mint = asset;
  const terms: PaymentTerms = {
    amount: '2000000',
    asset: mint,
    network,
    payTo: recipient,
    feePayer: feePayerAddress,
    memo,
  };
  const source = await associatedTokenAddress(payerAddress, mint);
  const destination = await associatedTokenAddress(recipient, mint);
  const amount = new Uint8Array(10);
  amount[0] = 12;
  new DataView(amount.buffer).setBigUint64(1, 2_000_000n, true);
  amount[9] = 6;
  const transfer = {
    programAddress: address(tokenProgram),
    accounts: [
      { address: address(source), role: AccountRole.WRITABLE },
      { address: address(mint), role: AccountRole.READONLY },
      { address: address(destination), role: AccountRole.WRITABLE },
      { address: address(payerAddress), role: AccountRole.READONLY_SIGNER },
    ],
    data: amount,
  };
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (value) => setTransactionMessageFeePayer(address(feePayerAddress), value),
    (value) =>
      setTransactionMessageLifetimeUsingBlockhash(
        {
          blockhash: blockhash(getBase58Decoder().decode(new Uint8Array(32).fill(4))),
          lastValidBlockHeight: 1n,
        },
        value,
      ),
    (value) =>
      appendTransactionMessageInstruction(
        { programAddress: address(computeBudgetProgram), data: new Uint8Array([2, 32, 78, 0, 0]) },
        value,
      ),
    (value) =>
      appendTransactionMessageInstruction(
        {
          programAddress: address(computeBudgetProgram),
          data: new Uint8Array([3, 1, 0, 0, 0, 0, 0, 0, 0]),
        },
        value,
      ),
    (value) => appendTransactionMessageInstruction(transfer, value),
    (value) =>
      appendTransactionMessageInstruction(
        { programAddress: address(memoProgram), data: new TextEncoder().encode(memo) },
        value,
      ),
  );
  const unsigned = new Uint8Array(getTransactionEncoder().encode(compileTransaction(message)));
  const original = transactionWithSignature(unsigned, payerAddress, 7);
  const payer: WalletAccount = {
    address: payerAddress,
    publicKey: new Uint8Array(32).fill(1),
    chains: ['solana:mainnet'],
    features: ['solana:signTransaction'],
  };
  return { original, payer, terms };
}

async function associatedTokenAddress(owner: string, mint: string): Promise<string> {
  const encoder = getAddressEncoder();
  const [derived] = await getProgramDerivedAddress({
    programAddress: address(associatedTokenProgram),
    seeds: [
      encoder.encode(address(owner)),
      encoder.encode(address(tokenProgram)),
      encoder.encode(address(mint)),
    ],
  });
  return derived;
}

function addLighthouse(
  transaction: Uint8Array,
  count: number,
  role: AccountRole = AccountRole.READONLY,
): Uint8Array {
  const message = decompile(transaction);
  const additions = [];
  for (let index = 0; index < count; index += 1) {
    additions.push({
      programAddress: address(LIGHTHOUSE_PROGRAM),
      accounts:
        role === AccountRole.READONLY
          ? []
          : [{ address: address(getBase58Decoder().decode(new Uint8Array(32).fill(8))), role }],
      data: new Uint8Array([index]),
    });
  }
  return encodeSigned({
    ...message,
    instructions: [...message.instructions, ...additions],
  } as ReturnType<typeof decompile>);
}

function appendInstruction(transaction: Uint8Array, programAddress: string): Uint8Array {
  const message = decompile(transaction);
  return encodeSigned({
    ...message,
    instructions: [
      ...message.instructions,
      { programAddress: address(programAddress), data: new Uint8Array([1]) },
    ],
  } as ReturnType<typeof decompile>);
}

function changeTransferAmount(transaction: Uint8Array): Uint8Array {
  const message = decompile(transaction);
  const instructions = [...message.instructions];
  const transfer = instructions[2]!;
  const data = new Uint8Array(transfer.data!);
  new DataView(data.buffer).setBigUint64(1, 3_000_000n, true);
  instructions[2] = { ...transfer, data };
  return encodeSigned({ ...message, instructions } as ReturnType<typeof decompile>);
}

function changeMemo(transaction: Uint8Array): Uint8Array {
  const message = decompile(transaction);
  const instructions = [...message.instructions];
  instructions[3] = { ...instructions[3]!, data: new TextEncoder().encode(`${memo}:changed`) };
  return encodeSigned({ ...message, instructions } as ReturnType<typeof decompile>);
}

function decompile(transaction: Uint8Array) {
  const decoded = getTransactionDecoder().decode(transaction);
  return decompileTransactionMessage(
    getCompiledTransactionMessageDecoder().decode(decoded.messageBytes),
  );
}

function encodeSigned(message: ReturnType<typeof decompile>): Uint8Array {
  const unsigned = new Uint8Array(getTransactionEncoder().encode(compileTransaction(message)));
  const payerAddress = getBase58Decoder().decode(new Uint8Array(32).fill(1));
  return transactionWithSignature(unsigned, payerAddress, 7);
}

function transactionWithSignature(
  transaction: Uint8Array,
  payerAddress: string,
  fill: number,
): Uint8Array {
  const decoded = getTransactionDecoder().decode(transaction);
  const signature = new Uint8Array(64).fill(fill);
  const signed = {
    ...decoded,
    signatures: { ...decoded.signatures, [address(payerAddress)]: signature as SignatureBytes },
  };
  return new Uint8Array(getTransactionEncoder().encode(signed));
}

function validate(fixture: Awaited<ReturnType<typeof paymentFixture>>, signed: Uint8Array) {
  return validateWalletSignedTransaction(fixture.original, signed, {
    payer: fixture.payer,
    terms: fixture.terms,
    verifySignature: async () => true,
  });
}
