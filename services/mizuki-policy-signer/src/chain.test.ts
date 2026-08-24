import {
  Keypair,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  SystemProgram,
  Transaction,
  TransactionMessage,
  VersionedTransaction,
  type FetchFn,
  type ParsedTransactionWithMeta,
  type SignatureStatus,
  type VersionedTransactionResponse,
} from '@solana/web3.js';
import { describe, expect, it, vi } from 'vitest';
import {
  assertMainnetGenesisHashes,
  assertInstructionProgramSequence,
  assertRpcSettlementIdentity,
  authorizedSettlementTransaction,
  boundedRpcFetch,
  ConsensusUsdPriceOracle,
  consensusCapacity,
  consensusTransactionState,
  findAuthorizedSettlementSignature,
  HttpUsdPriceOracle,
  immutableLoaderV3ProgramBytes,
  loaderV3ProgramDataAddress,
  matchesAuthorizedSettlement,
  sameSettlement,
  SOLANA_MAINNET_GENESIS_HASH,
  SolanaChainGateway,
  verifySettlementTransfer,
  type SettlementTransferPolicy,
} from './chain.js';
import { PolicyError } from './domain.js';
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from './token.js';

const payer = Keypair.generate().publicKey;
const treasury = Keypair.generate().publicKey;
const source = Keypair.generate().publicKey;
const destination = Keypair.generate().publicKey;
const mint = Keypair.generate().publicKey;

const policy: SettlementTransferPolicy = {
  treasuryWallet: treasury.toBase58(),
  treasuryTokenAccount: destination.toBase58(),
  mint: mint.toBase58(),
  decimals: 6,
  tokenProgramId: TOKEN_PROGRAM_ID.toBase58(),
};

describe('parsed settlement verification', () => {
  it('accepts one checked transfer whose amount equals the treasury net increase', () => {
    expect(verifySettlementTransfer(transaction(), policy)).toEqual({
      payer: payer.toBase58(),
      rawAmount: '2000000',
      decimals: 6,
    });
  });

  it('rejects a parsed transfer emitted by a different token program', () => {
    const value = transaction();
    value.transaction.message.instructions[0] = {
      ...(value.transaction.message.instructions[0] as object),
      programId: TOKEN_2022_PROGRAM_ID,
    } as never;
    expect(() => verifySettlementTransfer(value, policy)).toThrowError(
      expect.objectContaining({ code: 'invalid_settlement_form' }),
    );
  });

  it('rejects multiple incoming transfer instructions even when one is valid', () => {
    const value = transaction();
    value.transaction.message.instructions.push(
      structuredClone(value.transaction.message.instructions[0]) as never,
    );
    expect(() => verifySettlementTransfer(value, policy)).toThrowError(
      expect.objectContaining({ code: 'invalid_settlement_form' }),
    );
  });

  it('rejects a transfer followed by value moving back out of the treasury', () => {
    const value = transaction();
    value.meta!.postTokenBalances![1].uiTokenAmount.amount = '100000';
    expect(() => verifySettlementTransfer(value, policy)).toThrowError(
      expect.objectContaining({ code: 'settlement_value_mismatch' }),
    );
  });

  it.each([
    ['wrong destination owner', { owner: payer.toBase58() }],
    ['wrong destination mint', { mint: Keypair.generate().publicKey.toBase58() }],
    ['wrong destination decimals', { uiTokenAmount: tokenAmount('2100000', 9) }],
  ])('rejects %s metadata', (_name, patch) => {
    const value = transaction();
    Object.assign(value.meta!.postTokenBalances![1], patch);
    expect(() => verifySettlementTransfer(value, policy)).toThrowError(
      expect.objectContaining({ code: 'token_account_not_verified' }),
    );
  });

  it('rejects a transfer authority that is not the verified source owner', () => {
    const value = transaction();
    const instruction = value.transaction.message.instructions[0] as {
      parsed: { info: { authority: string } };
    };
    instruction.parsed.info.authority = treasury.toBase58();
    expect(() => verifySettlementTransfer(value, policy)).toThrowError(
      expect.objectContaining({ code: 'payer_not_verified' }),
    );
  });

  it('requires RPC transaction signature and finalized-status slot identity to agree', () => {
    const value = transaction();
    const status = {
      slot: value.slot,
      confirmations: null,
      err: null,
      confirmationStatus: 'finalized',
    } as SignatureStatus;
    expect(() => assertRpcSettlementIdentity(value, status, '6'.repeat(64))).not.toThrow();
    expect(() =>
      assertRpcSettlementIdentity(value, { ...status, slot: value.slot + 1 }, '6'.repeat(64)),
    ).toThrowError(expect.objectContaining({ code: 'rpc_inconsistent' }));
    expect(() => assertRpcSettlementIdentity(value, status, '7'.repeat(64))).toThrowError(
      expect.objectContaining({ code: 'rpc_inconsistent' }),
    );
  });

  it('requires independent providers to agree on every settlement fact', () => {
    const facts = {
      signature: '6'.repeat(64),
      payer: payer.toBase58(),
      recipient: treasury.toBase58(),
      mint: mint.toBase58(),
      rawAmount: '2000000',
      decimals: 6,
      finalized: true,
      succeeded: true,
      slot: 42,
      blockTimeUnixSeconds: 1,
    };
    expect(sameSettlement(facts, { ...facts })).toBe(true);
    expect(sameSettlement(facts, { ...facts, rawAmount: '2000001' })).toBe(false);
    expect(sameSettlement(facts, { ...facts, slot: 43 })).toBe(false);
  });
});

describe('payment authorization reconciliation', () => {
  it('matches only the exact payer-signed v0 message and client signature', () => {
    const fixture = settlementAuthorizationFixture();
    const expected = authorizedSettlementTransaction(fixture.authorization);

    expect(
      matchesAuthorizedSettlement(fixture.response, fixture.transactionSignature, expected),
    ).toBe(true);
    expect(
      matchesAuthorizedSettlement(
        {
          ...fixture.response,
          transaction: {
            ...fixture.response.transaction,
            signatures: [fixture.transactionSignature, '7'.repeat(64)],
          },
        },
        fixture.transactionSignature,
        expected,
      ),
    ).toBe(false);

    const altered = settlementAuthorizationFixture();
    expect(
      matchesAuthorizedSettlement(altered.response, altered.transactionSignature, expected),
    ).toBe(false);
    expect(() =>
      authorizedSettlementTransaction({
        ...fixture.authorization,
        feePayer: Keypair.generate().publicKey.toBase58(),
      }),
    ).toThrowError(expect.objectContaining({ code: 'payment_authorization_invalid' }));
  });

  it('rejects non-canonical, oversized, and fully signed authorization transactions', () => {
    const fixture = settlementAuthorizationFixture();
    expect(() =>
      authorizedSettlementTransaction({
        ...fixture.authorization,
        wireTransaction: `${fixture.authorization.wireTransaction}=`,
      }),
    ).toThrowError(expect.objectContaining({ code: 'payment_authorization_invalid' }));

    const oversized = Buffer.alloc(1_233).toString('base64');
    expect(() =>
      authorizedSettlementTransaction({ ...fixture.authorization, wireTransaction: oversized }),
    ).toThrowError(expect.objectContaining({ code: 'payment_authorization_invalid' }));

    expect(() =>
      authorizedSettlementTransaction({
        ...fixture.authorization,
        wireTransaction: fixture.fullySignedWireTransaction,
      }),
    ).toThrowError(expect.objectContaining({ code: 'payment_authorization_invalid' }));
  });

  it('paginates until it finds the exact authorized transaction', async () => {
    const fixture = settlementAuthorizationFixture();
    let page = 0;
    const connection = {
      getSignaturesForAddress: vi.fn(async () => {
        page += 1;
        if (page === 1) return settlementSignaturePage(256, 200, true);
        return [
          signatureInfo(fixture.transactionSignature, 150),
          ...settlementSignaturePage(7, 149, true),
        ];
      }),
      getTransactions: vi.fn(async (signatures: string[]) =>
        signatures.map((signature) =>
          signature === fixture.transactionSignature ? fixture.response : null,
        ),
      ),
    };
    const identity = authorizedSettlementTransaction(fixture.authorization);

    await expect(
      findAuthorizedSettlementSignature(connection as never, destination, {
        ...identity,
        rawAmount: fixture.authorization.rawAmount,
        notBeforeUnixSeconds: 100,
        notAfterUnixSeconds: 400,
      }),
    ).resolves.toBe(fixture.transactionSignature);
    expect(connection.getSignaturesForAddress).toHaveBeenCalledTimes(2);
    expect(connection.getSignaturesForAddress.mock.calls[1]?.[1]).toMatchObject({
      before: expect.any(String),
    });
  });

  it('returns retryable scan exhaustion instead of absence under a 4096-signature flood', async () => {
    let cursor = 0;
    const connection = {
      getSignaturesForAddress: vi.fn(async (_address, options: { limit: number }) => {
        const page = settlementSignaturePage(options.limit, 200, true, cursor);
        cursor += options.limit;
        return page;
      }),
      getTransactions: vi.fn(),
    };
    const fixture = settlementAuthorizationFixture();
    const identity = authorizedSettlementTransaction(fixture.authorization);

    await expect(
      findAuthorizedSettlementSignature(connection as never, destination, {
        ...identity,
        rawAmount: fixture.authorization.rawAmount,
        notBeforeUnixSeconds: 100,
        notAfterUnixSeconds: 400,
      }),
    ).rejects.toMatchObject({ code: 'settlement_scan_exhausted', retryable: true });
    expect(connection.getSignaturesForAddress).toHaveBeenCalledTimes(16);
    expect(connection.getTransactions).not.toHaveBeenCalled();
  });

  it('stops pagination at the admission time boundary and reports true absence', async () => {
    let page = 0;
    const connection = {
      getSignaturesForAddress: vi.fn(async () => {
        page += 1;
        return page === 1
          ? settlementSignaturePage(256, 200, true)
          : settlementSignaturePage(8, 99, true);
      }),
      getTransactions: vi.fn(),
    };
    const fixture = settlementAuthorizationFixture();
    const identity = authorizedSettlementTransaction(fixture.authorization);

    await expect(
      findAuthorizedSettlementSignature(connection as never, destination, {
        ...identity,
        rawAmount: fixture.authorization.rawAmount,
        notBeforeUnixSeconds: 100,
        notAfterUnixSeconds: 400,
      }),
    ).rejects.toMatchObject({ code: 'settlement_not_found' });
    expect(connection.getSignaturesForAddress).toHaveBeenCalledTimes(2);
  });

  it('preserves an authorization mismatch when both RPCs independently agree', async () => {
    const { gateway } = escrowGateway();
    const mismatch = new PolicyError(
      'payment_authorization_mismatch',
      'Settlement transaction does not match the admitted payment authorization',
      422,
    );
    const readAuthorizedSettlementFrom = vi.fn(async () => {
      throw mismatch;
    });
    (
      gateway as unknown as {
        readAuthorizedSettlementFrom: typeof readAuthorizedSettlementFrom;
      }
    ).readAuthorizedSettlementFrom = readAuthorizedSettlementFrom;
    const fixture = settlementAuthorizationFixture();
    const identity = authorizedSettlementTransaction(fixture.authorization);

    await expect(
      gateway.readAuthorizedSettlement(fixture.transactionSignature, {
        ...identity,
        rawAmount: fixture.authorization.rawAmount,
        notBeforeUnixSeconds: 100,
        notAfterUnixSeconds: 400,
      }),
    ).rejects.toBe(mismatch);
    expect(readAuthorizedSettlementFrom).toHaveBeenCalledTimes(2);
  });
});

describe('transaction form policy', () => {
  it('accepts only two canonical mainnet-beta genesis observations', () => {
    expect(() =>
      assertMainnetGenesisHashes(SOLANA_MAINNET_GENESIS_HASH, SOLANA_MAINNET_GENESIS_HASH),
    ).not.toThrow();
    expect(() =>
      assertMainnetGenesisHashes(
        SOLANA_MAINNET_GENESIS_HASH,
        'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG',
      ),
    ).toThrowError(expect.objectContaining({ code: 'rpc_wrong_cluster' }));
  });

  it('requires immutable loader-v3 program data and hashes only executable bytes', () => {
    const programDataAddress = Keypair.generate().publicKey;
    const program = Buffer.alloc(36);
    program.writeUInt32LE(2, 0);
    programDataAddress.toBuffer().copy(program, 4);
    expect(loaderV3ProgramDataAddress(program).equals(programDataAddress)).toBe(true);

    const executable = Buffer.from('approved-sbf-executable');
    const immutable = Buffer.alloc(45 + executable.length);
    immutable.writeUInt32LE(3, 0);
    immutable.writeBigUInt64LE(42n, 4);
    immutable.writeUInt8(0, 12);
    executable.copy(immutable, 45);
    expect(immutableLoaderV3ProgramBytes(immutable)).toEqual(executable);

    const mutable = Buffer.from(immutable);
    mutable.writeUInt8(1, 12);
    expect(() => immutableLoaderV3ProgramBytes(mutable)).toThrowError(
      expect.objectContaining({ code: 'escrow_program_mutable' }),
    );
  });

  it('accepts only the exact ordered instruction sequence', () => {
    const expected = [TOKEN_PROGRAM_ID.toBase58(), SystemProgram.programId.toBase58()];
    expect(() => assertInstructionProgramSequence([...expected], expected)).not.toThrow();
    expect(() =>
      assertInstructionProgramSequence([...expected, TOKEN_PROGRAM_ID.toBase58()], expected),
    ).toThrowError(expect.objectContaining({ code: 'transaction_form_not_allowed' }));
    expect(() => assertInstructionProgramSequence([...expected].reverse(), expected)).toThrowError(
      expect.objectContaining({ code: 'transaction_form_not_allowed' }),
    );
  });

  it('refuses a built-in program as the configured escrow program', () => {
    const refundSigner = Keypair.generate();
    const escrowSigner = Keypair.generate();
    expect(
      () =>
        new SolanaChainGateway({
          rpcUrl: 'http://127.0.0.1:8899',
          secondaryRpcUrl: 'http://127.0.0.1:8900',
          rpcTimeoutMs: 5_000,
          refundPrivateKeyJson: JSON.stringify([...refundSigner.secretKey]),
          escrowPrivateKeyJson: JSON.stringify([...escrowSigner.secretKey]),
          refundTreasury: refundSigner.publicKey.toBase58(),
          escrowAuthority: escrowSigner.publicKey.toBase58(),
          refundMint: mint.toBase58(),
          refundDecimals: 6,
          refundTokenProgram: 'spl-token',
          escrowProgramId: SystemProgram.programId.toBase58(),
          escrowProgramDataSha256: 'a'.repeat(64),
          solFeeReserveLamports: 1_000_000,
        }),
    ).toThrow('Escrow program must not be a built-in transaction program');
  });

  it('requires distinct refund and escrow authorities', () => {
    const signer = Keypair.generate();
    expect(
      () =>
        new SolanaChainGateway({
          rpcUrl: 'http://127.0.0.1:8899',
          secondaryRpcUrl: 'http://127.0.0.1:8900',
          rpcTimeoutMs: 5_000,
          refundPrivateKeyJson: JSON.stringify([...signer.secretKey]),
          escrowPrivateKeyJson: JSON.stringify([...signer.secretKey]),
          refundTreasury: signer.publicKey.toBase58(),
          escrowAuthority: signer.publicKey.toBase58(),
          refundMint: mint.toBase58(),
          refundDecimals: 6,
          refundTokenProgram: 'spl-token',
          escrowProgramId: Keypair.generate().publicKey.toBase58(),
          escrowProgramDataSha256: 'a'.repeat(64),
          solFeeReserveLamports: 1_000_000,
        }),
    ).toThrow('Refund and escrow authorities must use distinct keys');
  });

  it('encodes the fixed FUND ABI and canonical PDAs exactly', async () => {
    const { gateway, signer, program } = escrowGateway();
    const bountyDigest = 'b'.repeat(64);
    const prepared = await gateway.prepare({
      kind: 'escrow_reserve',
      intentId: 'reserve-op',
      bountyDigest,
      amountLamports: '123456789',
      expiresAtUnixSeconds: '1787428800',
      acceptanceHash: 'c'.repeat(64),
    });
    const transaction = Transaction.from(Buffer.from(prepared.wireTransaction, 'base64'));
    const instruction = transaction.instructions[0];
    const [state, stateBump] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-escrow'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );
    const [vault, vaultBump] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-vault'), state.toBuffer()],
      program,
    );
    const [guard, guardBump] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-guard'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );

    expect(instruction.data).toHaveLength(84);
    expect(instruction.data.readUInt8(0)).toBe(0);
    expect(instruction.data.subarray(1, 33).toString('hex')).toBe(bountyDigest);
    expect(instruction.data.readBigUInt64LE(33)).toBe(123456789n);
    expect(instruction.data.readBigInt64LE(41)).toBe(1787428800n);
    expect(instruction.data.subarray(49, 81).toString('hex')).toBe('c'.repeat(64));
    expect(instruction.data.readUInt8(81)).toBe(stateBump);
    expect(instruction.data.readUInt8(82)).toBe(vaultBump);
    expect(instruction.data.readUInt8(83)).toBe(guardBump);
    expect(instruction.keys.map((key) => key.pubkey.toBase58())).toEqual([
      signer.publicKey.toBase58(),
      state.toBase58(),
      vault.toBase58(),
      guard.toBase58(),
      SystemProgram.programId.toBase58(),
      SYSVAR_CLOCK_PUBKEY.toBase58(),
      SYSVAR_RENT_PUBKEY.toBase58(),
    ]);
    expect(prepared.derived).toEqual({
      escrowAddress: state.toBase58(),
      vaultAddress: vault.toBase58(),
      guardAddress: guard.toBase58(),
      bountyDigest,
    });
  });

  it('signs token refunds only with the distinct refund authority', async () => {
    const { gateway, refundSigner, signer: escrowSigner } = escrowGateway();
    const prepared = await gateway.prepare({
      kind: 'refund',
      intentId: 'refund-op',
      payer: payer.toBase58(),
      mint: mint.toBase58(),
      rawAmount: '2000000',
      decimals: 6,
    });
    const transaction = Transaction.from(Buffer.from(prepared.wireTransaction, 'base64'));

    expect(transaction.feePayer?.equals(refundSigner.publicKey)).toBe(true);
    expect(
      transaction.signatures.some(
        (entry) => entry.publicKey.equals(refundSigner.publicKey) && entry.signature,
      ),
    ).toBeTruthy();
    expect(
      transaction.signatures.some((entry) => entry.publicKey.equals(escrowSigner.publicKey)),
    ).toBe(false);
  });

  it('encodes BIND with only authority, state, and clock', async () => {
    const { gateway, signer, program } = escrowGateway();
    const bountyDigest = 'b'.repeat(64);
    const claimant = Keypair.generate().publicKey;
    const prepared = await gateway.prepare({
      kind: 'escrow_bind',
      intentId: 'bind-op',
      bountyDigest,
      claimantWallet: claimant.toBase58(),
      claimExpiresAtUnixSeconds: '1787601600',
      bindingEvidence: 'd'.repeat(64),
    });
    const instruction = Transaction.from(Buffer.from(prepared.wireTransaction, 'base64'))
      .instructions[0];
    const [state] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-escrow'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );
    const [guard] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-guard'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );

    expect(instruction.data).toHaveLength(105);
    expect(instruction.data.readUInt8(0)).toBe(1);
    expect(instruction.data.subarray(1, 33).toString('hex')).toBe(bountyDigest);
    expect(instruction.data.subarray(33, 65)).toEqual(claimant.toBuffer());
    expect(instruction.data.readBigInt64LE(65)).toBe(1787601600n);
    expect(instruction.data.subarray(73).toString('hex')).toBe('d'.repeat(64));
    expect(instruction.keys.map((key) => key.pubkey.toBase58())).toEqual([
      signer.publicKey.toBase58(),
      state.toBase58(),
      guard.toBase58(),
      SYSVAR_CLOCK_PUBKEY.toBase58(),
    ]);
  });

  it('encodes RELEASE and REFUND with no alternate program accounts', async () => {
    const { gateway, signer, program } = escrowGateway();
    const bountyDigest = 'b'.repeat(64);
    const claimant = Keypair.generate().publicKey;
    const [state] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-escrow'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );
    const [vault] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-vault'), state.toBuffer()],
      program,
    );
    const [guard] = PublicKey.findProgramAddressSync(
      [Buffer.from('mizuki-guard'), signer.publicKey.toBuffer(), Buffer.from(bountyDigest, 'hex')],
      program,
    );
    const release = await gateway.prepare({
      kind: 'escrow_release',
      intentId: 'release-op',
      bountyDigest,
      claimantWallet: claimant.toBase58(),
      resolutionEvidence: 'e'.repeat(64),
    });
    const refund = await gateway.prepare({
      kind: 'escrow_refund',
      intentId: 'refund-op',
      bountyDigest,
      resolutionEvidence: 'f'.repeat(64),
    });
    const releaseInstruction = Transaction.from(Buffer.from(release.wireTransaction, 'base64'))
      .instructions[0];
    const refundInstruction = Transaction.from(Buffer.from(refund.wireTransaction, 'base64'))
      .instructions[0];

    expect(releaseInstruction.data).toHaveLength(65);
    expect(releaseInstruction.data.readUInt8(0)).toBe(2);
    expect(releaseInstruction.data.subarray(33).toString('hex')).toBe('e'.repeat(64));
    expect(releaseInstruction.keys.map((key) => key.pubkey.toBase58())).toEqual([
      signer.publicKey.toBase58(),
      state.toBase58(),
      vault.toBase58(),
      guard.toBase58(),
      claimant.toBase58(),
      SYSVAR_CLOCK_PUBKEY.toBase58(),
    ]);
    expect(refundInstruction.data).toHaveLength(65);
    expect(refundInstruction.data.readUInt8(0)).toBe(3);
    expect(refundInstruction.data.subarray(33).toString('hex')).toBe('f'.repeat(64));
    expect(refundInstruction.keys.map((key) => key.pubkey.toBase58())).toEqual([
      signer.publicKey.toBase58(),
      state.toBase58(),
      vault.toBase58(),
      guard.toBase58(),
      SYSVAR_CLOCK_PUBKEY.toBase58(),
    ]);
  });
});

describe('independent RPC finality', () => {
  it('requires exact agreement on custody balances and rent facts', () => {
    const capacity = {
      refundRawAmount: '100000000',
      escrowLamports: '2000000000',
      stateRentLamports: '2000000',
      vaultRentLamports: '1000000',
      guardRentLamports: '1500000',
    };

    expect(consensusCapacity(capacity, { ...capacity })).toEqual(capacity);
    expect(() =>
      consensusCapacity(capacity, { ...capacity, refundRawAmount: '99999999' }),
    ).toThrowError(expect.objectContaining({ code: 'rpc_inconsistent', retryable: true }));
    expect(() =>
      consensusCapacity(capacity, { ...capacity, escrowLamports: '1999999999' }),
    ).toThrowError(expect.objectContaining({ code: 'rpc_inconsistent', retryable: true }));
  });

  it('reports terminal state only when providers agree', () => {
    expect(consensusTransactionState('finalized', 'finalized')).toBe('finalized');
    expect(consensusTransactionState('failed', 'failed')).toBe('failed');
    expect(consensusTransactionState('missing', 'missing')).toBe('missing');
    expect(consensusTransactionState('finalized', 'missing')).toBe('submitted');
    expect(consensusTransactionState('failed', 'missing')).toBe('submitted');
  });

  it('fails closed on contradictory terminal outcomes', () => {
    expect(() => consensusTransactionState('finalized', 'failed')).toThrowError(
      expect.objectContaining({ code: 'rpc_inconsistent' }),
    );
  });
});

describe('RPC transport bound', () => {
  it('forbids redirects before sending RPC requests', async () => {
    const fetcher = vi.fn(async (_input: Parameters<FetchFn>[0], init: Parameters<FetchFn>[1]) => {
      expect(init?.redirect).toBe('error');
      return new Response('{}', { status: 200 });
    }) as unknown as FetchFn;

    await expect(boundedRpcFetch(5_000, fetcher)('https://rpc.example')).resolves.toBeInstanceOf(
      Response,
    );
    expect(fetcher).toHaveBeenCalledOnce();
  });

  it('aborts a stalled HTTP request and returns a retryable policy error', async () => {
    const fetcher = vi.fn(
      (_input: Parameters<FetchFn>[0], init: Parameters<FetchFn>[1]) =>
        new Promise<never>((_resolve, reject) => {
          const signal = init?.signal as AbortSignal;
          signal.addEventListener('abort', () => reject(signal.reason), { once: true });
        }),
    ) as unknown as FetchFn;

    await expect(boundedRpcFetch(20, fetcher)('https://rpc.example')).rejects.toMatchObject({
      code: 'rpc_timeout',
      retryable: true,
    });
    expect(fetcher).toHaveBeenCalledOnce();
  });

  it('combines a caller cancellation with the transport deadline', async () => {
    const fetcher = vi.fn(
      (_input: Parameters<FetchFn>[0], init: Parameters<FetchFn>[1]) =>
        new Promise<never>((_resolve, reject) => {
          const signal = init?.signal as AbortSignal;
          signal.addEventListener('abort', () => reject(signal.reason), { once: true });
        }),
    ) as unknown as FetchFn;
    const controller = new AbortController();
    const request = boundedRpcFetch(1_000, fetcher)('https://rpc.example', {
      signal: controller.signal,
    });

    controller.abort();

    await expect(request).rejects.toMatchObject({ code: 'rpc_unavailable', retryable: true });
  });

  it('maps retryable HTTP service failures without web3 backoff', async () => {
    const fetcher = vi.fn(async () => new Response(null, { status: 429 })) as unknown as FetchFn;

    await expect(boundedRpcFetch(5_000, fetcher)('https://rpc.example')).rejects.toMatchObject({
      code: 'rpc_unavailable',
      retryable: true,
    });
    expect(fetcher).toHaveBeenCalledOnce();
  });
});

describe('price feed policy', () => {
  const now = new Date('2026-08-22T12:00:00.000Z').getTime();

  it('accepts a fresh bounded JSON observation', async () => {
    const oracle = priceOracle({
      priceUsdMicros: 150_000_000,
      observedAt: new Date(now - 1_000).toISOString(),
    });
    await expect(oracle.solUsd()).resolves.toMatchObject({ priceUsdMicros: 150_000_000 });
  });

  it('accepts the official Coinbase Exchange ticker response', async () => {
    const oracle = priceOracle(
      {
        trade_id: 42,
        price: '150.12345678',
        size: '1.0',
        time: new Date(now - 1_000).toISOString(),
        bid: '150.12',
        ask: '150.13',
        volume: '1000',
      },
      'https://api.exchange.coinbase.com/products/SOL-USD/ticker',
    );
    await expect(oracle.solUsd()).resolves.toEqual({
      priceUsdMicros: 150_123_456,
      observedAt: new Date(now - 1_000),
    });
  });

  it('accepts the official CoinGecko simple-price response', async () => {
    const oracle = priceOracle(
      {
        solana: {
          usd: 149.9876549,
          last_updated_at: Math.floor((now - 2_000) / 1_000),
        },
      },
      'https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd&include_last_updated_at=true&precision=6',
    );
    await expect(oracle.solUsd()).resolves.toEqual({
      priceUsdMicros: 149_987_654,
      observedAt: new Date(now - 2_000),
    });
  });

  it.each([
    [
      'Coinbase malformed price',
      'https://api.exchange.coinbase.com/products/SOL-USD/ticker',
      { price: 150, time: new Date(now).toISOString() },
      'price_invalid',
    ],
    [
      'Coinbase stale time',
      'https://api.exchange.coinbase.com/products/SOL-USD/ticker',
      { price: '150.00', time: new Date(now - 300_001).toISOString() },
      'price_stale',
    ],
    [
      'CoinGecko missing timestamp',
      'https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd&include_last_updated_at=true',
      { solana: { usd: 150 } },
      'price_invalid',
    ],
    [
      'CoinGecko stale timestamp',
      'https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd&include_last_updated_at=true',
      { solana: { usd: 150, last_updated_at: Math.floor((now - 301_000) / 1_000) } },
      'price_stale',
    ],
  ])('rejects a %s response', async (_name, url, body, code) => {
    await expect(priceOracle(body, url).solUsd()).rejects.toMatchObject({ code });
  });

  it.each([
    ['stale', new Date(now - 300_001).toISOString(), 'price_stale'],
    ['future', new Date(now + 5_001).toISOString(), 'price_stale'],
  ])('rejects a %s observation', async (_name, observedAt, code) => {
    const oracle = priceOracle({ priceUsdMicros: 150_000_000, observedAt });
    await expect(oracle.solUsd()).rejects.toMatchObject({ code });
  });

  it('rejects an out-of-bounds observation', async () => {
    const oracle = priceOracle({
      priceUsdMicros: 999_999,
      observedAt: new Date(now).toISOString(),
    });
    await expect(oracle.solUsd()).rejects.toMatchObject({ code: 'price_out_of_bounds' });
  });

  it('rejects oversized and non-JSON responses', async () => {
    const oversized = priceOracle('x'.repeat(2_049));
    const invalid = priceOracle('not-json');
    await expect(oversized.solUsd()).rejects.toMatchObject({ code: 'price_invalid' });
    await expect(invalid.solUsd()).rejects.toMatchObject({ code: 'price_invalid' });
  });

  function priceOracle(body: object | string, url = 'https://price.internal'): HttpUsdPriceOracle {
    const fetcher = (async () =>
      new Response(typeof body === 'string' ? body : JSON.stringify(body), {
        headers: { 'content-type': 'application/json' },
      })) as typeof fetch;
    return new HttpUsdPriceOracle(
      url,
      'test-price-token',
      1_000_000,
      1_000_000_000,
      300_000,
      fetcher,
      () => now,
    );
  }

  it('requires bounded agreement and uses the lower independent observation', async () => {
    const primaryObservedAt = new Date(now - 1_000);
    const secondaryObservedAt = new Date(now - 2_000);
    const oracle = new ConsensusUsdPriceOracle(
      {
        solUsd: async () => ({ priceUsdMicros: 150_000_000, observedAt: primaryObservedAt }),
      },
      {
        solUsd: async () => ({ priceUsdMicros: 147_000_000, observedAt: secondaryObservedAt }),
      },
      500,
    );

    await expect(oracle.solUsd()).resolves.toEqual({
      priceUsdMicros: 147_000_000,
      observedAt: secondaryObservedAt,
      observations: [
        {
          feed: 'primary',
          priceUsdMicros: 150_000_000,
          observedAt: primaryObservedAt,
        },
        {
          feed: 'secondary',
          priceUsdMicros: 147_000_000,
          observedAt: secondaryObservedAt,
        },
      ],
    });
  });

  it('fails closed when independent observations diverge', async () => {
    const observedAt = new Date(now);
    const oracle = new ConsensusUsdPriceOracle(
      { solUsd: async () => ({ priceUsdMicros: 150_000_000, observedAt }) },
      { solUsd: async () => ({ priceUsdMicros: 140_000_000, observedAt }) },
      500,
    );

    await expect(oracle.solUsd()).rejects.toMatchObject({ code: 'price_inconsistent' });
  });
});

function settlementAuthorizationFixture(): {
  authorization: {
    wireTransaction: string;
    feePayer: string;
    rawAmount: string;
    notBeforeUnixSeconds: number;
  };
  fullySignedWireTransaction: string;
  response: VersionedTransactionResponse;
  transactionSignature: string;
} {
  const feePayer = Keypair.generate();
  const client = Keypair.generate();
  const message = new TransactionMessage({
    payerKey: feePayer.publicKey,
    recentBlockhash: Keypair.generate().publicKey.toBase58(),
    instructions: [
      SystemProgram.transfer({
        fromPubkey: client.publicKey,
        toPubkey: destination,
        lamports: 1,
      }),
    ],
  }).compileToV0Message();
  const partial = new VersionedTransaction(message);
  partial.sign([client]);
  const wireTransaction = Buffer.from(partial.serialize()).toString('base64');
  const finalized = VersionedTransaction.deserialize(partial.serialize());
  finalized.sign([feePayer]);
  const signatures = finalized.signatures.map(testBase58Encode);
  const transactionSignature = signatures[0]!;
  return {
    authorization: {
      wireTransaction,
      feePayer: feePayer.publicKey.toBase58(),
      rawAmount: '2000000',
      notBeforeUnixSeconds: 100,
    },
    fullySignedWireTransaction: Buffer.from(finalized.serialize()).toString('base64'),
    response: {
      slot: 42,
      blockTime: 150,
      meta: { err: null },
      transaction: {
        message: finalized.message,
        signatures,
      },
      version: 0,
    } as unknown as VersionedTransactionResponse,
    transactionSignature,
  };
}

function settlementSignaturePage(length: number, blockTime: number, failed: boolean, offset = 0) {
  return Array.from({ length }, (_, index) =>
    signatureInfo(`signature-${offset + index}`, blockTime, failed),
  );
}

function signatureInfo(signature: string, blockTime: number, failed = false) {
  return {
    signature,
    slot: 42,
    err: failed ? ({ InstructionError: [0, 'Custom'] } as never) : null,
    memo: null,
    blockTime,
    confirmationStatus: 'finalized' as const,
  };
}

function testBase58Encode(bytes: Uint8Array): string {
  const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  let value = 0n;
  for (const byte of bytes) value = value * 256n + BigInt(byte);
  let encoded = '';
  while (value > 0n) {
    encoded = alphabet[Number(value % 58n)] + encoded;
    value /= 58n;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    encoded = `1${encoded}`;
  }
  return encoded || '1';
}

function transaction(): ParsedTransactionWithMeta {
  const signature = '6'.repeat(64);
  return {
    slot: 42,
    blockTime: 1,
    version: 'legacy',
    transaction: {
      signatures: [signature],
      message: {
        accountKeys: [
          account(payer, true, true),
          account(source, false, true),
          account(destination, false, true),
          account(mint, false, false),
        ],
        instructions: [
          {
            program: 'spl-token',
            programId: TOKEN_PROGRAM_ID,
            parsed: {
              type: 'transferChecked',
              info: {
                source: source.toBase58(),
                destination: destination.toBase58(),
                mint: mint.toBase58(),
                authority: payer.toBase58(),
                tokenAmount: tokenAmount('2000000', 6),
              },
            },
          },
        ],
        recentBlockhash: '7'.repeat(32),
      },
    },
    meta: {
      err: null,
      fee: 5_000,
      preBalances: [1, 1, 1, 1],
      postBalances: [1, 1, 1, 1],
      innerInstructions: [],
      logMessages: [],
      preTokenBalances: [balance(1, mint, payer, '5000000'), balance(2, mint, treasury, '100000')],
      postTokenBalances: [
        balance(1, mint, payer, '3000000'),
        balance(2, mint, treasury, '2100000'),
      ],
      rewards: [],
      status: { Ok: null },
    },
  } as unknown as ParsedTransactionWithMeta;
}

function escrowGateway(): {
  gateway: SolanaChainGateway;
  signer: Keypair;
  refundSigner: Keypair;
  program: PublicKey;
} {
  const signer = Keypair.generate();
  const refundSigner = Keypair.generate();
  const program = Keypair.generate().publicKey;
  const gateway = new SolanaChainGateway({
    rpcUrl: 'http://127.0.0.1:8899',
    secondaryRpcUrl: 'http://127.0.0.1:8900',
    rpcTimeoutMs: 5_000,
    refundPrivateKeyJson: JSON.stringify([...refundSigner.secretKey]),
    escrowPrivateKeyJson: JSON.stringify([...signer.secretKey]),
    refundTreasury: refundSigner.publicKey.toBase58(),
    escrowAuthority: signer.publicKey.toBase58(),
    refundMint: mint.toBase58(),
    refundDecimals: 6,
    refundTokenProgram: 'spl-token',
    escrowProgramId: program.toBase58(),
    escrowProgramDataSha256: 'a'.repeat(64),
    solFeeReserveLamports: 1_000_000,
  });
  const internals = gateway as unknown as {
    readCapacityConsensus: () => Promise<{
      refundRawAmount: string;
      escrowLamports: string;
      stateRentLamports: string;
      vaultRentLamports: string;
      guardRentLamports: string;
    }>;
    readRefundCapacityConsensus: () => Promise<string>;
    verifyMainnetCluster: () => Promise<void>;
    verifyEscrowProgram: () => Promise<void>;
    connection: {
      getLatestBlockhash: () => Promise<{ blockhash: string; lastValidBlockHeight: number }>;
    };
  };
  internals.verifyMainnetCluster = async () => undefined;
  internals.verifyEscrowProgram = async () => undefined;
  internals.readCapacityConsensus = async () => ({
    refundRawAmount: '1000000000',
    escrowLamports: '100000000000',
    stateRentLamports: '2000000',
    vaultRentLamports: '1000000',
    guardRentLamports: '1500000',
  });
  internals.readRefundCapacityConsensus = async () => '1000000000';
  internals.connection.getLatestBlockhash = async () => ({
    blockhash: Keypair.generate().publicKey.toBase58(),
    lastValidBlockHeight: 100,
  });
  return { gateway, signer, refundSigner, program };
}

function account(pubkey: PublicKey, signer: boolean, writable: boolean) {
  return { pubkey, signer, writable, source: 'transaction' as const };
}

function balance(accountIndex: number, tokenMint: PublicKey, owner: PublicKey, amount: string) {
  return {
    accountIndex,
    mint: tokenMint.toBase58(),
    owner: owner.toBase58(),
    uiTokenAmount: tokenAmount(amount, 6),
    programId: TOKEN_PROGRAM_ID.toBase58(),
  };
}

function tokenAmount(amount: string, decimals: number) {
  return { amount, decimals, uiAmount: Number(amount) / 10 ** decimals, uiAmountString: amount };
}
