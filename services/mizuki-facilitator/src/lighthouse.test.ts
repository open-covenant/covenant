import { describe, expect, it } from 'vitest';
import {
  AccountRole,
  address,
  generateKeyPairSigner,
  getAddressEncoder,
  getProgramDerivedAddress,
  getBase58Decoder,
  getTransactionEncoder,
  compileTransaction,
  createTransactionMessage,
  appendTransactionMessageInstruction,
  blockhash,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  type Address,
} from '@solana/kit';
import { LIGHTHOUSE_PROGRAM, verifyLighthouseTransaction } from './lighthouse.js';
import {
  ata,
  buildPayment,
  computeLimit,
  computePrice,
  guard,
  memo,
  transferChecked,
  AMOUNT,
  MEMO_TEXT,
  USDC,
  ASSOCIATED_TOKEN,
} from './test-fixtures.js';

describe('wallet-guarded payment verification', () => {
  it('accepts an unguarded payment', async () => {
    const { transaction, terms, payer } = await buildPayment(({ source, destination, payer }) => [
      computeLimit(),
      computePrice(),
      transferChecked(source, destination, payer),
      memo(),
    ]);

    await expect(verifyLighthouseTransaction(transaction, terms)).resolves.toMatchObject({
      ok: true,
      payer,
    });
  });

  it('accepts the Phantom shape that brackets the transfer with guards', async () => {
    const { transaction, terms, payer } = await buildPayment(({ source, destination, payer }) => [
      computeLimit(),
      computePrice(),
      guard(17),
      guard(52),
      guard(16),
      transferChecked(source, destination, payer),
      memo(),
      guard(27),
    ]);

    await expect(verifyLighthouseTransaction(transaction, terms)).resolves.toMatchObject({
      ok: true,
      payer,
    });
  });

  it('lets a guard read the payer and fee payer accounts', async () => {
    const { transaction, terms } = await buildPayment(
      ({ source, destination, payer, feePayer }) => [
        computeLimit(),
        computePrice(),
        guard(17, [
          { address: payer, role: AccountRole.READONLY_SIGNER },
          { address: feePayer, role: AccountRole.READONLY },
        ]),
        transferChecked(source, destination, payer),
        memo(),
      ],
    );

    await expect(verifyLighthouseTransaction(transaction, terms)).resolves.toMatchObject({
      ok: true,
    });
  });

  it('lets a guard write only to its payer-derived memory account', async () => {
    const payerSigner = await generateKeyPairSigner();
    const encoder = getAddressEncoder();
    const [memory] = await getProgramDerivedAddress({
      programAddress: address(LIGHTHOUSE_PROGRAM),
      seeds: [
        new TextEncoder().encode('memory'),
        encoder.encode(payerSigner.address),
        new Uint8Array([0]),
      ],
    });

    const { transaction, terms } = await buildPayment(({ source, destination, payer }) => [
      computeLimit(),
      computePrice(),
      guard(17, [{ address: memory, role: AccountRole.WRITABLE }]),
      transferChecked(source, destination, payer),
      memo(),
    ]);

    // The memory account belongs to a different payer than this payment's, so
    // the write must still be refused.
    await expect(verifyLighthouseTransaction(transaction, terms)).resolves.toMatchObject({
      ok: false,
      reason: expect.stringContaining('write access widened'),
    });
  });

  it.each([
    [
      'a changed transfer amount',
      ({
        source,
        destination,
        payer,
      }: {
        source: Address;
        destination: Address;
        payer: Address;
      }) => [
        computeLimit(),
        computePrice(),
        transferChecked(source, destination, payer, AMOUNT + 1n),
        memo(),
      ],
      'transfer amount',
    ],
    [
      'a changed memo',
      ({
        source,
        destination,
        payer,
      }: {
        source: Address;
        destination: Address;
        payer: Address;
      }) => [
        computeLimit(),
        computePrice(),
        transferChecked(source, destination, payer),
        memo('mizuki:someone-elses-quote'),
      ],
      'memo does not match',
    ],
    [
      'more guards than accepted',
      ({
        source,
        destination,
        payer,
      }: {
        source: Address;
        destination: Address;
        payer: Address;
      }) => [
        computeLimit(),
        computePrice(),
        ...Array.from({ length: 7 }, () => guard(9)),
        transferChecked(source, destination, payer),
        memo(),
      ],
      'too many wallet guard instructions',
    ],
    [
      'an extra unknown program',
      ({
        source,
        destination,
        payer,
      }: {
        source: Address;
        destination: Address;
        payer: Address;
      }) => [
        computeLimit(),
        computePrice(),
        transferChecked(source, destination, payer),
        memo(),
        { programAddress: ASSOCIATED_TOKEN, accounts: [], data: new Uint8Array([1]) },
      ],
      'four instructions',
    ],
    [
      'a compute unit price above the accepted maximum',
      ({
        source,
        destination,
        payer,
      }: {
        source: Address;
        destination: Address;
        payer: Address;
      }) => [
        computeLimit(),
        computePrice(100_001n),
        transferChecked(source, destination, payer),
        memo(),
      ],
      'compute unit price',
    ],
  ])('rejects %s', async (_name, build, reason) => {
    const { transaction, terms } = await buildPayment(build);

    await expect(verifyLighthouseTransaction(transaction, terms)).resolves.toMatchObject({
      ok: false,
      reason: expect.stringContaining(reason),
    });
  });

  it('rejects a payment for a different recipient', async () => {
    const { transaction, terms } = await buildPayment(({ source, destination, payer }) => [
      computeLimit(),
      computePrice(),
      transferChecked(source, destination, payer),
      memo(),
    ]);
    const other = await generateKeyPairSigner();

    await expect(
      verifyLighthouseTransaction(transaction, { ...terms, payTo: other.address }),
    ).resolves.toMatchObject({ ok: false, reason: expect.stringContaining('transfer accounts') });
  });

  it('rejects an unsigned payment', async () => {
    const payerSigner = await generateKeyPairSigner();
    const feePayerSigner = await generateKeyPairSigner();
    const recipient = await generateKeyPairSigner();
    const source = await ata(payerSigner.address, USDC);
    const destination = await ata(recipient.address, USDC);
    const message = [
      computeLimit(),
      computePrice(),
      transferChecked(source, destination, payerSigner.address),
      memo(),
    ].reduce(
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
    const unsigned = Buffer.from(
      getTransactionEncoder().encode(compileTransaction(message)),
    ).toString('base64');

    await expect(
      verifyLighthouseTransaction(unsigned, {
        amount: String(AMOUNT),
        asset: USDC,
        payTo: recipient.address,
        feePayer: feePayerSigner.address,
        memo: MEMO_TEXT,
      }),
    ).resolves.toMatchObject({ ok: false, reason: expect.stringContaining('payer signature') });
  });

  it('rejects an undecodable payload', async () => {
    await expect(
      verifyLighthouseTransaction('not-base64-transaction', {
        amount: '1',
        asset: USDC,
        payTo: USDC,
        feePayer: USDC,
      }),
    ).resolves.toMatchObject({ ok: false });
  });
});
