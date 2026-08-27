import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  assertIsSignatureBytes,
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
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  assertPaymentBalance,
  createPaymentFetch,
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
  delete process.env.NEXT_PUBLIC_SOLANA_RPC_URL;
  vi.unstubAllGlobals();
});

describe('x402 quote policy', () => {
  it('does not consume a POST body while observing payment headers', async () => {
    const payer = await paymentFixture();
    const request = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const outgoing = new Request(input, init);
      expect(await outgoing.json()).toEqual({ quote_id: 'quote-1' });
      return Response.json({ ok: true });
    });
    const paidFetch = createPaymentFetch({
      account: payer.payer,
      feature: {
        version: '1.0.0',
        supportedTransactionVersions: [0],
        signTransaction: vi.fn(),
      },
      quotePayment: { x402Version: 2, accepts: [requirements] },
      quoteAmount: requirements.amount,
      request,
    });

    const response = await paidFetch('https://mizuki.example/v1/jobs', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ quote_id: 'quote-1' }),
    });

    expect(response.status).toBe(200);
    expect(request).toHaveBeenCalledOnce();
  });

  it('preserves the body through the 402 challenge, wallet signature, and paid retry', async () => {
    const { payer, accepted, paymentRequired, rpc } = await livePaymentFixture();
    const stages: string[] = [];
    const resourceRequests: Array<{ body: unknown; idempotencyKey: string | null; paid: boolean }> =
      [];
    const request = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const outgoing = new Request(input, init);
      resourceRequests.push({
        body: await outgoing.clone().json(),
        idempotencyKey: outgoing.headers.get('idempotency-key'),
        paid: outgoing.headers.has('payment-signature'),
      });
      if (resourceRequests.length === 1) {
        return paymentChallenge(paymentRequired);
      }
      return Response.json({ id: 'job-1', state: 'paid' }, { status: 201 });
    });
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: accepted.amount,
      onStage(stage) {
        stages.push(stage);
      },
      request,
    });

    const response = await paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'attempt-1' },
      body: JSON.stringify({ quote_id: 'quote-1', payment_attempt_id: 'attempt-1' }),
    });

    expect(response.status).toBe(201);
    expect(resourceRequests).toEqual([
      {
        body: { quote_id: 'quote-1', payment_attempt_id: 'attempt-1' },
        idempotencyKey: 'attempt-1',
        paid: false,
      },
      {
        body: { quote_id: 'quote-1', payment_attempt_id: 'attempt-1' },
        idempotencyKey: 'attempt-1',
        paid: true,
      },
    ]);
    expect(stages).toEqual(['wallet_opened', 'wallet_signed', 'submitting']);
    expect(rpc).toHaveBeenCalledTimes(2);
  });

  it('does not open the wallet when the resource does not request payment', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const sign = vi.spyOn(payer.feature, 'signTransaction');
    const request = vi.fn(async () => Response.json({ error: 'unavailable' }, { status: 503 }));
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      request,
    });

    const response = await paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
      method: 'POST',
      body: '{}',
    });

    expect(response.status).toBe(503);
    expect(sign).not.toHaveBeenCalled();
    expect(request).toHaveBeenCalledOnce();
  });

  it('rejects a changed live challenge before opening the wallet', async () => {
    const { payer, accepted, paymentRequired } = await livePaymentFixture();
    const sign = vi.spyOn(payer.feature, 'signTransaction');
    const changed = {
      ...paymentRequired,
      accepts: [
        {
          ...accepted,
          payTo: getBase58Decoder().decode(new Uint8Array(32).fill(7)),
        },
      ],
    };
    const request = vi.fn(async () => paymentChallenge(changed));
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: accepted.amount,
      request,
    });

    await expect(
      paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
        method: 'POST',
        body: '{}',
      }),
    ).rejects.toThrow('payment challenge does not match');
    expect(sign).not.toHaveBeenCalled();
    expect(request).toHaveBeenCalledOnce();
  });

  it('does not silently retry or reopen the wallet when the paid response is lost', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const sign = vi.spyOn(payer.feature, 'signTransaction');
    const stages: string[] = [];
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(paymentChallenge(paymentRequired))
      .mockRejectedValueOnce(new TypeError('Failed to fetch'));
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      onStage(stage) {
        stages.push(stage);
      },
      request,
    });

    await expect(
      paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
        method: 'POST',
        body: '{}',
      }),
    ).rejects.toThrow('Failed to fetch');
    expect(sign).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledTimes(2);
    expect(stages).toEqual(['wallet_opened', 'wallet_signed', 'submitting']);
  });

  it('does not reopen the wallet when the paid retry still returns 402', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const sign = vi.spyOn(payer.feature, 'signTransaction');
    const request = vi.fn(async () => paymentChallenge(paymentRequired));
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      request,
    });

    const response = await paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
      method: 'POST',
      body: '{}',
    });

    expect(response.status).toBe(402);
    expect(sign).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledTimes(2);
  });

  it('does not let diagnostic stage reporting delay transaction submission', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const stages: string[] = [];
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(paymentChallenge(paymentRequired))
      .mockResolvedValueOnce(Response.json({ id: 'job-1' }, { status: 201 }));
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      onStage(stage) {
        stages.push(stage);
        return new Promise(() => undefined);
      },
      request,
      stageConfirmationTimeoutMs: 1,
    });

    const response = await paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
      method: 'POST',
      body: '{}',
    });

    expect(response.status).toBe(201);
    expect(request).toHaveBeenCalledTimes(2);
    expect(stages).toEqual(['wallet_opened', 'wallet_signed', 'submitting']);
  });

  it('times out a lost paid response so server recovery can begin', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(paymentChallenge(paymentRequired))
      .mockImplementationOnce(
        (_input, init) =>
          new Promise<Response>((_resolve, reject) => {
            init?.signal?.addEventListener(
              'abort',
              () => reject(init.signal?.reason ?? new DOMException('Aborted', 'AbortError')),
              { once: true },
            );
          }),
      );
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: payer.feature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      request,
      paidRequestTimeoutMs: 10,
    });

    await expect(
      paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
        method: 'POST',
        body: '{}',
      }),
    ).rejects.toThrow('timed out');
    expect(request).toHaveBeenCalledTimes(2);
  });

  it('preserves the wallet rejection code across the x402 wrapper', async () => {
    const { payer, paymentRequired } = await livePaymentFixture();
    const rejectedFeature = {
      ...payer.feature,
      async signTransaction(): Promise<never> {
        throw new Error('User rejected request');
      },
    };
    const paidFetch = createPaymentFetch({
      account: payer.account,
      feature: rejectedFeature,
      quotePayment: paymentRequired,
      quoteAmount: requirements.amount,
      request: vi.fn(async () => paymentChallenge(paymentRequired)),
    });

    let cause: unknown;
    try {
      await paidFetch('https://mizuki.example/api/mizuki/v1/jobs', {
        method: 'POST',
        body: '{}',
      });
    } catch (error) {
      cause = error;
    }

    expect(cause).toMatchObject({ code: 'wallet_rejected' });
    expect(paymentPreparationError(cause, '2 USDC')).toContain(
      'Payment was cancelled in your wallet',
    );
  });

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

  it('checks the canonical USDC balance before opening the wallet', async () => {
    const request = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({
        jsonrpc: '2.0',
        id: 'mizuki-payment-balance',
        result: { value: { amount: '4000000', decimals: 6, uiAmountString: '4' } },
      }),
    );

    await expect(assertPaymentBalance('1'.repeat(32), '2000000', request)).resolves.toBeUndefined();

    const [, init] = request.mock.calls[0]!;
    expect(JSON.parse(String(init?.body))).toMatchObject({
      method: 'getTokenAccountBalance',
      params: [expect.any(String), { commitment: 'confirmed' }],
    });
  });

  it('stops an underfunded wallet before transaction signing', async () => {
    const request = vi.fn(async () =>
      Response.json({
        jsonrpc: '2.0',
        id: 'mizuki-payment-balance',
        result: { value: { amount: '1999999', decimals: 6, uiAmountString: '1.999999' } },
      }),
    );

    await expect(assertPaymentBalance('1'.repeat(32), '2000000', request)).rejects.toMatchObject({
      code: 'insufficient_funds',
    });
  });

  it('distinguishes a missing USDC account from an unavailable RPC', async () => {
    const missing = vi.fn(async () =>
      Response.json({
        jsonrpc: '2.0',
        id: 'mizuki-payment-balance',
        error: { code: -32602, message: 'Invalid param: could not find account' },
      }),
    );
    const unavailable = vi.fn(async () =>
      Response.json({ error: 'Solana RPC is unavailable' }, { status: 502 }),
    );

    await expect(assertPaymentBalance('1'.repeat(32), '2000000', missing)).rejects.toMatchObject({
      code: 'insufficient_funds',
    });
    await expect(
      assertPaymentBalance('1'.repeat(32), '2000000', unavailable),
    ).rejects.toMatchObject({ code: 'rpc_unavailable' });
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

  it.each([
    [
      'Failed to create payment payload: The wallet rejected the payment request',
      'Payment was cancelled in your wallet',
    ],
    [
      'Failed to create payment payload: The wallet disconnected before authorizing the payment',
      'The payment wallet disconnected',
    ],
    [
      'Failed to create payment payload: The wallet changed protected payment instructions',
      'The wallet could not safely authorize this payment',
    ],
  ])('translates wrapped wallet failures without exposing SDK internals', (failure, expected) => {
    const message = paymentPreparationError(new Error(failure), '2 USDC');

    expect(message).toContain(expected);
    expect(message).not.toContain('payment payload');
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

async function signingWallet() {
  const keys = await crypto.subtle.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify']);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey));
  const payerAddress = getBase58Decoder().decode(publicKey);
  const account: WalletAccount = {
    address: payerAddress,
    publicKey,
    chains: ['solana:mainnet'],
    features: ['solana:signTransaction'],
  };
  const feature = {
    version: '1.0.0' as const,
    supportedTransactionVersions: [0] as const,
    async signTransaction(...inputs: readonly { transaction: Uint8Array }[]) {
      return Promise.all(
        inputs.map(async (input) => {
          const transaction = getTransactionDecoder().decode(input.transaction);
          const message = Uint8Array.from(transaction.messageBytes);
          const signature = new Uint8Array(
            await crypto.subtle.sign('Ed25519', keys.privateKey, message),
          );
          assertIsSignatureBytes(signature);
          return {
            signedTransaction: new Uint8Array(
              getTransactionEncoder().encode({
                ...transaction,
                signatures: {
                  ...transaction.signatures,
                  [address(payerAddress)]: signature,
                },
              }),
            ),
          };
        }),
      );
    },
  };
  return { account, feature };
}

function mintAccountResponse(id: number): Response {
  const data = new Uint8Array(82);
  data[44] = 6;
  data[45] = 1;
  return Response.json({
    jsonrpc: '2.0',
    id,
    result: {
      context: { slot: 1 },
      value: {
        data: [Buffer.from(data).toString('base64'), 'base64'],
        executable: false,
        lamports: 1,
        owner: tokenProgram,
        rentEpoch: 0,
        space: data.byteLength,
      },
    },
  });
}

function blockhashResponse(id: number): Response {
  return Response.json({
    jsonrpc: '2.0',
    id,
    result: {
      context: { slot: 1 },
      value: {
        blockhash: getBase58Decoder().decode(new Uint8Array(32).fill(4)),
        lastValidBlockHeight: 10,
      },
    },
  });
}

async function livePaymentFixture() {
  const payer = await signingWallet();
  const recipient = getBase58Decoder().decode(new Uint8Array(32).fill(3));
  const sponsor = getBase58Decoder().decode(new Uint8Array(32).fill(2));
  const accepted = {
    ...requirements,
    payTo: recipient,
    extra: { feePayer: sponsor, memo },
  };
  const paymentRequired = {
    x402Version: 2,
    resource: {
      url: 'https://mizuki.example/v1/jobs?quote_id=quote-1',
      description: 'Start one fixed-price maintenance job',
      mimeType: 'application/json',
    },
    accepts: [accepted],
    extensions: {},
  };
  const rpc = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = (await new Request(input, init).json()) as { id: number; method: string };
    if (call.method === 'getAccountInfo') return mintAccountResponse(call.id);
    if (call.method === 'getLatestBlockhash') return blockhashResponse(call.id);
    return Response.json({ jsonrpc: '2.0', id: call.id, error: { code: -32601 } });
  });
  vi.stubGlobal('fetch', rpc);
  process.env.NEXT_PUBLIC_SOLANA_RPC_URL = 'https://rpc.mizuki.example';
  return { accepted, payer, paymentRequired, rpc };
}

function paymentChallenge(paymentRequired: unknown): Response {
  return Response.json(paymentRequired, {
    status: 402,
    headers: {
      'payment-required': Buffer.from(JSON.stringify(paymentRequired)).toString('base64'),
    },
  });
}
