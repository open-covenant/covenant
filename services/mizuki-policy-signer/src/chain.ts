import { createHash } from 'node:crypto';
import {
  Connection,
  Keypair,
  PublicKey,
  SendTransactionError,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  type ParsedInstruction,
  type ParsedTransactionWithMeta,
  type SignatureStatus,
} from '@solana/web3.js';
import { z } from 'zod';
import type {
  ChainOperation,
  PreparedTransaction,
  SettlementFacts,
  TransactionState,
} from './domain.js';
import { PolicyError } from './domain.js';
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  associatedTokenAddress,
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
} from './token.js';

const MEMO_PROGRAM_ID = new PublicKey('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');
const UPGRADEABLE_LOADER_ID = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');
const ESCROW_STATE_BYTES = 236;
const ESCROW_VAULT_BYTES = 40;
const ESCROW_GUARD_BYTES = 108;

export interface ChainGateway {
  readSettlement(signature: string): Promise<SettlementFacts>;
  prepare(operation: ChainOperation): Promise<PreparedTransaction>;
  broadcast(prepared: PreparedTransaction): Promise<void>;
  transactionState(signature: string): Promise<TransactionState>;
  blockHeight(): Promise<number>;
  unixTime(): Promise<number>;
  refundCapacity(): Promise<string>;
  capacity(): Promise<ChainCapacity>;
  health(): Promise<ChainHealthEvidence>;
}

export interface ChainCapacity {
  refundRawAmount: string;
  escrowLamports: string;
  stateRentLamports: string;
  vaultRentLamports: string;
  guardRentLamports: string;
}

export interface ChainHealthEvidence extends ChainCapacity {
  rpcProviders: 2;
  escrowProgramId: string;
  escrowProgramDataSha256: string;
  escrowProgramImmutable: true;
}

export interface UsdPrice {
  priceUsdMicros: number;
  observedAt: Date;
  observations?: Array<{
    feed: 'primary' | 'secondary';
    priceUsdMicros: number;
    observedAt: Date;
  }>;
}

export interface UsdPriceOracle {
  solUsd(): Promise<UsdPrice>;
}

export interface SolanaGatewayConfig {
  rpcUrl: string;
  secondaryRpcUrl: string;
  refundPrivateKeyJson: string;
  escrowPrivateKeyJson: string;
  refundTreasury: string;
  escrowAuthority: string;
  refundMint: string;
  refundDecimals: number;
  refundTokenProgram: 'spl-token';
  escrowProgramId: string;
  escrowProgramDataSha256: string;
  solFeeReserveLamports: number;
}

export class SolanaChainGateway implements ChainGateway {
  private readonly connection: Connection;
  private readonly secondaryConnection: Connection;
  private readonly refundSigner: Keypair;
  private readonly escrowSigner: Keypair;
  private readonly refundTreasury: PublicKey;
  private readonly refundMint: PublicKey;
  private readonly refundDecimals: number;
  private readonly refundTokenProgram: PublicKey;
  private readonly escrowProgramId: PublicKey;
  private readonly escrowProgramDataSha256: string;
  private readonly solFeeReserveLamports: bigint;

  constructor(config: SolanaGatewayConfig) {
    this.connection = new Connection(config.rpcUrl, 'finalized');
    this.secondaryConnection = new Connection(config.secondaryRpcUrl, 'finalized');
    this.refundSigner = parseKeypair(config.refundPrivateKeyJson);
    this.escrowSigner = parseKeypair(config.escrowPrivateKeyJson);
    this.refundTreasury = new PublicKey(config.refundTreasury);
    const escrowAuthority = new PublicKey(config.escrowAuthority);
    if (!this.refundSigner.publicKey.equals(this.refundTreasury)) {
      throw new Error('Refund signer key must control the configured refund treasury');
    }
    if (!this.escrowSigner.publicKey.equals(escrowAuthority)) {
      throw new Error('Escrow signer key must match the configured escrow authority');
    }
    if (this.refundSigner.publicKey.equals(this.escrowSigner.publicKey)) {
      throw new Error('Refund and escrow authorities must use distinct keys');
    }
    this.refundMint = new PublicKey(config.refundMint);
    this.refundDecimals = config.refundDecimals;
    this.refundTokenProgram = tokenProgram(config.refundTokenProgram);
    this.escrowProgramId = new PublicKey(config.escrowProgramId);
    if (!/^[a-f0-9]{64}$/.test(config.escrowProgramDataSha256)) {
      throw new Error('Escrow program data hash is invalid');
    }
    this.escrowProgramDataSha256 = config.escrowProgramDataSha256;
    this.solFeeReserveLamports = BigInt(config.solFeeReserveLamports);
    const reservedPrograms = [
      SystemProgram.programId,
      TOKEN_PROGRAM_ID,
      TOKEN_2022_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID,
      MEMO_PROGRAM_ID,
    ];
    if (reservedPrograms.some((program) => program.equals(this.escrowProgramId))) {
      throw new Error('Escrow program must not be a built-in transaction program');
    }
  }

  async readSettlement(signature: string): Promise<SettlementFacts> {
    const results = await Promise.allSettled([
      this.readSettlementFrom(this.connection, signature),
      this.readSettlementFrom(this.secondaryConnection, signature),
    ]);
    if (results.every((result) => result.status === 'rejected')) {
      const errors = results.map((result) => (result as PromiseRejectedResult).reason);
      if (
        errors.every(
          (error) => error instanceof PolicyError && error.code === 'settlement_not_found',
        )
      ) {
        throw errors[0];
      }
      throw new PolicyError(
        'rpc_unavailable',
        'Independent RPC providers could not verify settlement facts',
        503,
        true,
      );
    }
    if (results.some((result) => result.status === 'rejected')) {
      throw new PolicyError(
        'rpc_inconsistent',
        'Independent RPC providers disagree on settlement availability',
        503,
        true,
      );
    }
    const [primary, secondary] = results.map(
      (result) => (result as PromiseFulfilledResult<SettlementFacts>).value,
    );
    if (!sameSettlement(primary, secondary)) {
      throw new PolicyError(
        'rpc_inconsistent',
        'Independent RPC providers disagree on settlement facts',
        503,
        true,
      );
    }
    return primary;
  }

  private async readSettlementFrom(
    connection: Connection,
    signature: string,
  ): Promise<SettlementFacts> {
    const [transaction, status] = await Promise.all([
      connection.getParsedTransaction(signature, {
        commitment: 'finalized',
        maxSupportedTransactionVersion: 0,
      }),
      connection.getSignatureStatuses([signature], { searchTransactionHistory: true }),
    ]);
    if (!transaction || !status.value[0]) {
      throw new PolicyError(
        'settlement_not_found',
        'Settlement transaction was not found',
        422,
        true,
      );
    }

    const confirmation = status.value[0];
    assertRpcSettlementIdentity(transaction, confirmation, signature);
    const treasuryTokenAccount = associatedTokenAddress(
      this.refundMint,
      this.refundTreasury,
      this.refundTokenProgram,
    ).toBase58();
    const transfer = verifySettlementTransfer(transaction, {
      treasuryWallet: this.refundTreasury.toBase58(),
      treasuryTokenAccount,
      mint: this.refundMint.toBase58(),
      decimals: this.refundDecimals,
      tokenProgramId: this.refundTokenProgram.toBase58(),
    });
    if (transaction.blockTime == null) {
      throw new PolicyError(
        'settlement_time_unavailable',
        'Finalized settlement time could not be verified',
        503,
        true,
      );
    }
    return {
      signature,
      payer: transfer.payer,
      recipient: this.refundTreasury.toBase58(),
      mint: this.refundMint.toBase58(),
      rawAmount: transfer.rawAmount,
      decimals: transfer.decimals,
      finalized: confirmation.confirmationStatus === 'finalized',
      succeeded: transaction.meta?.err === null && confirmation.err === null,
      slot: transaction.slot,
      blockTimeUnixSeconds: transaction.blockTime,
    };
  }

  async prepare(operation: ChainOperation): Promise<PreparedTransaction> {
    const transaction = new Transaction();
    const signer = operation.kind === 'refund' ? this.refundSigner : this.escrowSigner;
    let derived: Record<string, string> = {};

    if (operation.kind !== 'refund') await this.verifyEscrowProgram();

    const capacity =
      operation.kind === 'refund'
        ? {
            refundRawAmount: await this.refundCapacity(),
            escrowLamports: '0',
            stateRentLamports: '0',
            vaultRentLamports: '0',
            guardRentLamports: '0',
          }
        : await this.capacity();

    if (operation.kind === 'refund') {
      const mint = new PublicKey(operation.mint);
      if (!mint.equals(this.refundMint) || operation.decimals !== this.refundDecimals) {
        throw new PolicyError('asset_not_allowed', 'Refund asset is not allowed', 403);
      }
      if (BigInt(capacity.refundRawAmount) < BigInt(operation.rawAmount)) {
        throw new PolicyError(
          'refund_pool_insufficient',
          'Protected refund pool is insufficient',
          503,
          true,
        );
      }
      const payer = new PublicKey(operation.payer);
      const source = associatedTokenAddress(
        mint,
        this.refundSigner.publicKey,
        this.refundTokenProgram,
      );
      const destination = associatedTokenAddress(mint, payer, this.refundTokenProgram);
      transaction.add(
        createAssociatedTokenAccountIdempotentInstruction(
          this.refundSigner.publicKey,
          destination,
          payer,
          mint,
          this.refundTokenProgram,
        ),
        createTransferCheckedInstruction(
          source,
          mint,
          destination,
          this.refundSigner.publicKey,
          BigInt(operation.rawAmount),
          operation.decimals,
          this.refundTokenProgram,
        ),
      );
    } else {
      const escrowBalance = BigInt(capacity.escrowLamports);
      if (escrowBalance < this.solFeeReserveLamports) {
        throw new PolicyError(
          'escrow_pool_insufficient',
          'Escrow fee reserve is insufficient',
          503,
          true,
        );
      }

      if (operation.kind === 'escrow_reserve') {
        const principal = parsePositiveU64(operation.amountLamports, 'escrow amount');
        const required =
          principal +
          BigInt(capacity.stateRentLamports) +
          BigInt(capacity.vaultRentLamports) +
          BigInt(capacity.guardRentLamports) +
          this.solFeeReserveLamports;
        if (escrowBalance < required) {
          throw new PolicyError(
            'escrow_pool_insufficient',
            'Escrow pool cannot fund this reservation',
            503,
            true,
          );
        }
      }
      const escrowInstruction = buildEscrowInstruction(
        this.escrowProgramId,
        this.escrowSigner.publicKey,
        operation,
      );
      transaction.add(escrowInstruction.instruction);
      derived = escrowInstruction.derived;
    }

    transaction.add(
      new TransactionInstruction({
        programId: MEMO_PROGRAM_ID,
        keys: [],
        data: Buffer.from(`mizuki:${operation.intentId}`),
      }),
    );
    this.assertAllowed(transaction, operation.kind, operation.intentId);

    const { blockhash, lastValidBlockHeight } =
      await this.connection.getLatestBlockhash('finalized');
    transaction.recentBlockhash = blockhash;
    transaction.feePayer = signer.publicKey;
    transaction.sign(signer);
    if (!transaction.signature) throw new Error('Transaction was not signed');

    return {
      signature: base58Encode(transaction.signature),
      wireTransaction: transaction.serialize().toString('base64'),
      lastValidBlockHeight,
      derived,
    };
  }

  async broadcast(prepared: PreparedTransaction): Promise<void> {
    let signature: string;
    try {
      signature = await this.connection.sendRawTransaction(
        Buffer.from(prepared.wireTransaction, 'base64'),
        { skipPreflight: false, maxRetries: 3 },
      );
    } catch (error) {
      if (error instanceof SendTransactionError) {
        throw new PolicyError(
          'transaction_preflight_failed',
          'Transaction failed deterministic preflight validation',
          409,
        );
      }
      throw error;
    }
    if (signature !== prepared.signature) {
      throw new Error('RPC returned an unexpected transaction signature');
    }
  }

  async transactionState(signature: string): Promise<TransactionState> {
    const [primary, secondary] = await Promise.all([
      this.readTransactionState(this.connection, signature),
      this.readTransactionState(this.secondaryConnection, signature),
    ]);
    return consensusTransactionState(primary, secondary);
  }

  async blockHeight(): Promise<number> {
    const heights = await Promise.all([
      this.connection.getBlockHeight('finalized'),
      this.secondaryConnection.getBlockHeight('finalized'),
    ]);
    return Math.min(...heights);
  }

  async unixTime(): Promise<number> {
    const [primary, secondary] = await Promise.all([
      this.readUnixTime(this.connection),
      this.readUnixTime(this.secondaryConnection),
    ]);
    if (Math.abs(primary - secondary) > 30) {
      throw new PolicyError(
        'rpc_inconsistent',
        'Independent RPC providers disagree on finalized chain time',
        503,
        true,
      );
    }
    return Math.max(primary, secondary);
  }

  async capacity(): Promise<ChainCapacity> {
    const [primary, secondary] = await Promise.all([
      this.readCapacityFrom(this.connection),
      this.readCapacityFrom(this.secondaryConnection),
    ]);
    return consensusCapacity(primary, secondary);
  }

  async refundCapacity(): Promise<string> {
    const [primary, secondary] = await Promise.all([
      this.readRefundCapacityFrom(this.connection),
      this.readRefundCapacityFrom(this.secondaryConnection),
    ]);
    if (primary !== secondary) {
      throw new PolicyError(
        'rpc_inconsistent',
        'Independent RPC providers disagree on refund custody',
        503,
        true,
      );
    }
    return primary;
  }

  async health(): Promise<ChainHealthEvidence> {
    const [, , , capacity] = await Promise.all([
      this.connection.getLatestBlockhash('finalized'),
      this.secondaryConnection.getLatestBlockhash('finalized'),
      this.verifyEscrowProgram(),
      this.capacity(),
    ]);
    return {
      ...capacity,
      rpcProviders: 2,
      escrowProgramId: this.escrowProgramId.toBase58(),
      escrowProgramDataSha256: this.escrowProgramDataSha256,
      escrowProgramImmutable: true,
    };
  }

  private async readCapacityFrom(connection: Connection): Promise<ChainCapacity> {
    const [
      refundRawAmount,
      escrowLamports,
      stateRentLamports,
      vaultRentLamports,
      guardRentLamports,
    ] = await Promise.all([
      this.readRefundCapacityFrom(connection),
      connection.getBalance(this.escrowSigner.publicKey, 'finalized'),
      connection.getMinimumBalanceForRentExemption(ESCROW_STATE_BYTES, 'finalized'),
      connection.getMinimumBalanceForRentExemption(ESCROW_VAULT_BYTES, 'finalized'),
      connection.getMinimumBalanceForRentExemption(ESCROW_GUARD_BYTES, 'finalized'),
    ]);
    return {
      refundRawAmount,
      escrowLamports: String(escrowLamports),
      stateRentLamports: String(stateRentLamports),
      vaultRentLamports: String(vaultRentLamports),
      guardRentLamports: String(guardRentLamports),
    };
  }

  private async readRefundCapacityFrom(connection: Connection): Promise<string> {
    const refundAccount = associatedTokenAddress(
      this.refundMint,
      this.refundTreasury,
      this.refundTokenProgram,
    );
    const tokenAccount = await connection.getParsedAccountInfo(refundAccount, 'finalized');
    const value = tokenAccount.value;
    const data = value?.data;
    if (
      !value ||
      !value.owner.equals(this.refundTokenProgram) ||
      !data ||
      !('parsed' in data) ||
      data.program !== 'spl-token' ||
      data.parsed?.type !== 'account' ||
      data.parsed?.info?.mint !== this.refundMint.toBase58() ||
      data.parsed?.info?.owner !== this.refundTreasury.toBase58() ||
      data.parsed?.info?.tokenAmount?.decimals !== this.refundDecimals ||
      !/^\d+$/.test(data.parsed?.info?.tokenAmount?.amount ?? '')
    ) {
      throw new PolicyError(
        'refund_pool_invalid',
        'Protected refund token account could not be verified',
        503,
        true,
      );
    }
    return data.parsed.info.tokenAmount.amount;
  }

  private async verifyEscrowProgram(): Promise<void> {
    const deployments = await Promise.all([
      this.readProgramDeployment(this.connection),
      this.readProgramDeployment(this.secondaryConnection),
    ]);
    if (deployments[0] !== deployments[1] || deployments[0] !== this.escrowProgramDataSha256) {
      throw new PolicyError(
        'escrow_program_mismatch',
        'Escrow program deployment does not match the pinned finalized hash',
        503,
        true,
      );
    }
  }

  private async readProgramDeployment(connection: Connection): Promise<string> {
    const program = await connection.getAccountInfo(this.escrowProgramId, 'finalized');
    if (!program?.executable || !program.owner.equals(UPGRADEABLE_LOADER_ID)) {
      throw new PolicyError(
        'escrow_program_unavailable',
        'Configured escrow program is not an executable loader-v3 program at finalized state',
        503,
        true,
      );
    }
    const programDataAddress = loaderV3ProgramDataAddress(program.data);
    const programData = await connection.getAccountInfo(programDataAddress, 'finalized');
    if (
      !programData ||
      programData.executable ||
      !programData.owner.equals(UPGRADEABLE_LOADER_ID)
    ) {
      throw new PolicyError(
        'escrow_program_invalid',
        'Escrow program data account is invalid',
        503,
        true,
      );
    }
    return createHash('sha256')
      .update(immutableLoaderV3ProgramBytes(programData.data))
      .digest('hex');
  }

  private async readTransactionState(
    connection: Connection,
    signature: string,
  ): Promise<TransactionState> {
    const result = await connection.getSignatureStatuses([signature], {
      searchTransactionHistory: true,
    });
    const status = result.value[0];
    if (!status) return 'missing';
    if (status.err) return 'failed';
    return status.confirmationStatus === 'finalized' ? 'finalized' : 'submitted';
  }

  private async readUnixTime(connection: Connection): Promise<number> {
    const slot = await connection.getSlot('finalized');
    const unixTime = await connection.getBlockTime(slot);
    if (unixTime === null) {
      throw new PolicyError('rpc_unavailable', 'Finalized chain time is unavailable', 503, true);
    }
    return unixTime;
  }

  private assertAllowed(
    transaction: Transaction,
    kind: ChainOperation['kind'],
    intentId: string,
  ): void {
    const expectedPrograms =
      kind === 'refund'
        ? [
            ASSOCIATED_TOKEN_PROGRAM_ID.toBase58(),
            this.refundTokenProgram.toBase58(),
            MEMO_PROGRAM_ID.toBase58(),
          ]
        : [this.escrowProgramId.toBase58(), MEMO_PROGRAM_ID.toBase58()];
    assertInstructionProgramSequence(
      transaction.instructions.map((instruction) => instruction.programId.toBase58()),
      expectedPrograms,
    );
    const memo = transaction.instructions.at(-1);
    if (!memo || memo.keys.length !== 0 || !memo.data.equals(Buffer.from(`mizuki:${intentId}`))) {
      throw new PolicyError(
        'transaction_form_not_allowed',
        'Transaction intent memo is invalid',
        403,
      );
    }
  }
}

export interface SettlementTransferPolicy {
  treasuryWallet: string;
  treasuryTokenAccount: string;
  mint: string;
  decimals: number;
  tokenProgramId: string;
}

type EscrowChainOperation = Exclude<ChainOperation, { kind: 'refund' }>;

export function buildEscrowInstruction(
  programId: PublicKey,
  authority: PublicKey,
  operation: EscrowChainOperation,
): { instruction: TransactionInstruction; derived: Record<string, string> } {
  const bountyDigest = decodeHash(operation.bountyDigest, 'bounty digest');
  const [state, stateBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-escrow'), authority.toBuffer(), bountyDigest],
    programId,
  );
  const [vault, vaultBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-vault'), state.toBuffer()],
    programId,
  );
  const [guard, guardBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-guard'), authority.toBuffer(), bountyDigest],
    programId,
  );
  const derived = {
    escrowAddress: state.toBase58(),
    vaultAddress: vault.toBase58(),
    guardAddress: guard.toBase58(),
    bountyDigest: operation.bountyDigest,
  };

  if (operation.kind === 'escrow_reserve') {
    return {
      instruction: new TransactionInstruction({
        programId,
        keys: [
          { pubkey: authority, isSigner: true, isWritable: true },
          { pubkey: state, isSigner: false, isWritable: true },
          { pubkey: vault, isSigner: false, isWritable: true },
          { pubkey: guard, isSigner: false, isWritable: true },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
          { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
        ],
        data: encodeEscrowFundData({
          bountyDigest: operation.bountyDigest,
          amountLamports: operation.amountLamports,
          expiresAtUnixSeconds: operation.expiresAtUnixSeconds,
          acceptanceHash: operation.acceptanceHash,
          stateBump,
          vaultBump,
          guardBump,
        }),
      }),
      derived,
    };
  }

  if (operation.kind === 'escrow_bind') {
    return {
      instruction: new TransactionInstruction({
        programId,
        keys: [
          { pubkey: authority, isSigner: true, isWritable: false },
          { pubkey: state, isSigner: false, isWritable: true },
          { pubkey: guard, isSigner: false, isWritable: true },
          { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
        ],
        data: encodeEscrowBindData(operation),
      }),
      derived,
    };
  }

  const keys =
    operation.kind === 'escrow_release'
      ? [
          { pubkey: authority, isSigner: true, isWritable: true },
          { pubkey: state, isSigner: false, isWritable: true },
          { pubkey: vault, isSigner: false, isWritable: true },
          { pubkey: guard, isSigner: false, isWritable: true },
          {
            pubkey: new PublicKey(requiredValue(operation.claimantWallet, 'claimant wallet')),
            isSigner: false,
            isWritable: true,
          },
          { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
        ]
      : [
          { pubkey: authority, isSigner: true, isWritable: true },
          { pubkey: state, isSigner: false, isWritable: true },
          { pubkey: vault, isSigner: false, isWritable: true },
          { pubkey: guard, isSigner: false, isWritable: true },
          { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
        ];
  return {
    instruction: new TransactionInstruction({
      programId,
      keys,
      data: encodeEscrowResolutionData(operation),
    }),
    derived,
  };
}

export function encodeEscrowFundData(input: {
  bountyDigest: string;
  amountLamports: string;
  expiresAtUnixSeconds: string;
  acceptanceHash: string;
  stateBump: number;
  vaultBump: number;
  guardBump: number;
}): Buffer {
  const data = Buffer.alloc(84);
  data.writeUInt8(0, 0);
  decodeHash(input.bountyDigest, 'bounty digest').copy(data, 1);
  data.writeBigUInt64LE(parsePositiveU64(input.amountLamports, 'escrow amount'), 33);
  data.writeBigInt64LE(parseI64(input.expiresAtUnixSeconds, 'offer expiry'), 41);
  decodeHash(input.acceptanceHash, 'acceptance commitment').copy(data, 49);
  data.writeUInt8(byte(input.stateBump, 'state bump'), 81);
  data.writeUInt8(byte(input.vaultBump, 'vault bump'), 82);
  data.writeUInt8(byte(input.guardBump, 'guard bump'), 83);
  return data;
}

export function encodeEscrowBindData(input: {
  bountyDigest: string;
  claimantWallet: string;
  claimExpiresAtUnixSeconds: string;
  bindingEvidence: string;
}): Buffer {
  const data = Buffer.alloc(105);
  data.writeUInt8(1, 0);
  decodeHash(input.bountyDigest, 'bounty digest').copy(data, 1);
  new PublicKey(input.claimantWallet).toBuffer().copy(data, 33);
  data.writeBigInt64LE(parseI64(input.claimExpiresAtUnixSeconds, 'claim expiry'), 65);
  decodeHash(input.bindingEvidence, 'binding evidence').copy(data, 73);
  return data;
}

export function encodeEscrowResolutionData(input: {
  kind: 'escrow_release' | 'escrow_refund';
  bountyDigest: string;
  resolutionEvidence: string;
}): Buffer {
  const data = Buffer.alloc(65);
  data.writeUInt8(input.kind === 'escrow_release' ? 2 : 3, 0);
  decodeHash(input.bountyDigest, 'bounty digest').copy(data, 1);
  decodeHash(input.resolutionEvidence, 'resolution evidence').copy(data, 33);
  return data;
}

const transferInfoSchema = z
  .object({
    source: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    destination: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    authority: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    tokenAmount: z
      .object({
        amount: z.string().regex(/^\d+$/),
        decimals: z.number().int().min(0).max(18),
      })
      .passthrough(),
  })
  .passthrough();

export function assertRpcSettlementIdentity(
  transaction: ParsedTransactionWithMeta,
  confirmation: SignatureStatus,
  signature: string,
): void {
  if (
    confirmation.slot !== transaction.slot ||
    !transaction.transaction.signatures.includes(signature)
  ) {
    throw new PolicyError(
      'rpc_inconsistent',
      'RPC returned inconsistent settlement identity or slot data',
      503,
      true,
    );
  }
}

export function sameSettlement(left: SettlementFacts, right: SettlementFacts): boolean {
  return (
    left.signature === right.signature &&
    left.payer === right.payer &&
    left.recipient === right.recipient &&
    left.mint === right.mint &&
    left.rawAmount === right.rawAmount &&
    left.decimals === right.decimals &&
    left.finalized === right.finalized &&
    left.succeeded === right.succeeded &&
    left.slot === right.slot &&
    left.blockTimeUnixSeconds === right.blockTimeUnixSeconds
  );
}

export function loaderV3ProgramDataAddress(data: Buffer): PublicKey {
  if (data.length !== 36 || data.readUInt32LE(0) !== 2) {
    throw new PolicyError(
      'escrow_program_invalid',
      'Loader-v3 program metadata is invalid',
      503,
      true,
    );
  }
  return new PublicKey(data.subarray(4, 36));
}

export function immutableLoaderV3ProgramBytes(data: Buffer): Buffer {
  const metadataBytes = 45;
  if (data.length <= metadataBytes || data.readUInt32LE(0) !== 3) {
    throw new PolicyError(
      'escrow_program_invalid',
      'Loader-v3 program-data metadata is invalid',
      503,
      true,
    );
  }
  if (data.readUInt8(12) !== 0) {
    throw new PolicyError(
      'escrow_program_mutable',
      'Escrow program still has an upgrade authority',
      503,
      true,
    );
  }
  return data.subarray(metadataBytes);
}

export function consensusTransactionState(
  primary: TransactionState,
  secondary: TransactionState,
): TransactionState {
  if (
    (primary === 'finalized' && secondary === 'failed') ||
    (primary === 'failed' && secondary === 'finalized')
  ) {
    throw new PolicyError(
      'rpc_inconsistent',
      'Independent RPC providers disagree on transaction outcome',
      503,
      true,
    );
  }
  if (primary === secondary) return primary;
  return 'submitted';
}

export function consensusCapacity(primary: ChainCapacity, secondary: ChainCapacity): ChainCapacity {
  if (
    primary.refundRawAmount !== secondary.refundRawAmount ||
    primary.escrowLamports !== secondary.escrowLamports ||
    primary.stateRentLamports !== secondary.stateRentLamports ||
    primary.vaultRentLamports !== secondary.vaultRentLamports ||
    primary.guardRentLamports !== secondary.guardRentLamports
  ) {
    throw new PolicyError(
      'rpc_inconsistent',
      'Independent RPC providers disagree on signer custody or rent facts',
      503,
      true,
    );
  }
  return { ...primary };
}

export function verifySettlementTransfer(
  transaction: ParsedTransactionWithMeta,
  policy: SettlementTransferPolicy,
): { payer: string; rawAmount: string; decimals: number } {
  const instructions = [
    ...transaction.transaction.message.instructions,
    ...(transaction.meta?.innerInstructions?.flatMap((group) => group.instructions) ?? []),
  ];
  const treasuryTransfers = instructions.filter(
    (instruction): instruction is ParsedInstruction =>
      'parsed' in instruction &&
      ['transfer', 'transferChecked'].includes(instruction.parsed?.type) &&
      instruction.parsed?.info?.destination === policy.treasuryTokenAccount,
  );
  if (treasuryTransfers.length !== 1) {
    throw new PolicyError(
      'invalid_settlement_form',
      'Settlement must contain exactly one token transfer to the refund treasury',
      422,
    );
  }

  const transfer = treasuryTransfers[0];
  if (
    !transfer.programId.equals(new PublicKey(policy.tokenProgramId)) ||
    transfer.program !== 'spl-token' ||
    transfer.parsed?.type !== 'transferChecked' ||
    transfer.parsed?.info?.mint !== policy.mint
  ) {
    throw new PolicyError(
      'invalid_settlement_form',
      'Settlement transfer does not use the approved token instruction form',
      422,
    );
  }

  const parsedInfo = transferInfoSchema.safeParse(transfer.parsed.info);
  if (!parsedInfo.success) {
    throw new PolicyError('invalid_settlement_form', 'Settlement transfer fields are invalid', 422);
  }
  const info = parsedInfo.data;
  const sourceIndex = accountIndex(transaction, info.source);
  const destinationIndex = accountIndex(transaction, policy.treasuryTokenAccount);
  if (sourceIndex < 0 || destinationIndex < 0 || sourceIndex === destinationIndex) {
    throw new PolicyError('invalid_settlement_form', 'Settlement token accounts are invalid', 422);
  }

  const sourceBalance = transaction.meta?.preTokenBalances?.find(
    (balance) => balance.accountIndex === sourceIndex,
  );
  const destinationBefore = transaction.meta?.preTokenBalances?.find(
    (balance) => balance.accountIndex === destinationIndex,
  );
  const destinationAfter = transaction.meta?.postTokenBalances?.find(
    (balance) => balance.accountIndex === destinationIndex,
  );
  if (!validTokenBalance(sourceBalance, policy.mint, policy.decimals)) {
    throw new PolicyError(
      'token_account_not_verified',
      'Settlement token account ownership, mint, or decimals could not be verified',
      422,
    );
  }
  if (
    !validTokenBalance(destinationBefore, policy.mint, policy.decimals, policy.treasuryWallet) ||
    !validTokenBalance(destinationAfter, policy.mint, policy.decimals, policy.treasuryWallet)
  ) {
    throw new PolicyError(
      'token_account_not_verified',
      'Settlement token account ownership, mint, or decimals could not be verified',
      422,
    );
  }

  const payer = sourceBalance.owner!;
  const payerSigned = transaction.transaction.message.accountKeys.some(
    (account) => account.signer && account.pubkey.toBase58() === payer,
  );
  if (!payerSigned || info.authority !== payer) {
    throw new PolicyError(
      'payer_not_verified',
      'Settlement token owner did not authorize the transfer',
      422,
    );
  }
  if (info.tokenAmount.decimals !== policy.decimals || BigInt(info.tokenAmount.amount) <= 0n) {
    throw new PolicyError('invalid_settlement_amount', 'Settlement amount is invalid', 422);
  }

  const netIncrease =
    BigInt(destinationAfter.uiTokenAmount.amount) - BigInt(destinationBefore.uiTokenAmount.amount);
  if (netIncrease !== BigInt(info.tokenAmount.amount)) {
    throw new PolicyError(
      'settlement_value_mismatch',
      'Treasury net token increase does not equal the settlement transfer',
      422,
    );
  }
  return { payer, rawAmount: info.tokenAmount.amount, decimals: info.tokenAmount.decimals };
}

export function assertInstructionProgramSequence(actual: string[], expected: string[]): void {
  if (
    actual.length !== expected.length ||
    actual.some((program, index) => program !== expected[index])
  ) {
    throw new PolicyError(
      'transaction_form_not_allowed',
      'Transaction instruction sequence is not allowlisted',
      403,
    );
  }
}

function accountIndex(transaction: ParsedTransactionWithMeta, address: string): number {
  return transaction.transaction.message.accountKeys.findIndex(
    (account) => account.pubkey.toBase58() === address,
  );
}

type ParsedTokenBalance = NonNullable<
  NonNullable<ParsedTransactionWithMeta['meta']>['preTokenBalances']
>[number];
type VerifiedTokenBalance = ParsedTokenBalance & { owner: string };

function validTokenBalance(
  balance: ParsedTokenBalance | undefined,
  mint: string,
  decimals: number,
  owner?: string,
): balance is VerifiedTokenBalance {
  return Boolean(
    balance &&
    balance.mint === mint &&
    balance.uiTokenAmount.decimals === decimals &&
    (!owner || balance.owner === owner),
  );
}

const priceResponseSchema = z
  .object({
    priceUsdMicros: z.number().int().positive(),
    observedAt: z.string().datetime({ offset: true }),
  })
  .strict();

export class HttpUsdPriceOracle implements UsdPriceOracle {
  constructor(
    private readonly url: string,
    private readonly token: string | undefined,
    private readonly minPrice: number,
    private readonly maxPrice: number,
    private readonly fetcher: typeof fetch = fetch,
    private readonly now: () => number = Date.now,
  ) {}

  async solUsd(): Promise<UsdPrice> {
    let response: Response;
    try {
      response = await this.fetcher(this.url, {
        headers: {
          accept: 'application/json',
          ...(this.token ? { authorization: `Bearer ${this.token}` } : {}),
        },
        redirect: 'error',
        signal: AbortSignal.timeout(5_000),
      });
    } catch {
      throw new PolicyError('price_unavailable', 'Price service is unavailable', 503, true);
    }
    if (!response.ok)
      throw new PolicyError('price_unavailable', 'Price service is unavailable', 503, true);
    if (response.headers.get('content-type')?.split(';')[0]?.trim() !== 'application/json') {
      throw new PolicyError(
        'price_invalid',
        'Price service returned an invalid content type',
        503,
        true,
      );
    }
    const parsed = priceResponseSchema.safeParse(await readLimitedJson(response, 2_048));
    if (!parsed.success)
      throw new PolicyError('price_invalid', 'Price service returned invalid data', 503, true);
    const observedAt = new Date(parsed.data.observedAt);
    const age = this.now() - observedAt.getTime();
    if (age < -5_000 || age > 60_000) {
      throw new PolicyError(
        'price_stale',
        'Price observation is outside the allowed window',
        503,
        true,
      );
    }
    if (parsed.data.priceUsdMicros < this.minPrice || parsed.data.priceUsdMicros > this.maxPrice) {
      throw new PolicyError(
        'price_out_of_bounds',
        'Price observation is outside safety bounds',
        503,
        true,
      );
    }
    return { priceUsdMicros: parsed.data.priceUsdMicros, observedAt };
  }
}

export class ConsensusUsdPriceOracle implements UsdPriceOracle {
  constructor(
    private readonly primary: UsdPriceOracle,
    private readonly secondary: UsdPriceOracle,
    private readonly maxDivergenceBps: number,
  ) {
    if (!Number.isInteger(maxDivergenceBps) || maxDivergenceBps < 1 || maxDivergenceBps > 1_000) {
      throw new Error('Price divergence limit must be between 1 and 1000 basis points');
    }
  }

  async solUsd(): Promise<UsdPrice> {
    const [primary, secondary] = await Promise.all([
      this.primary.solUsd(),
      this.secondary.solUsd(),
    ]);
    const lower = Math.min(primary.priceUsdMicros, secondary.priceUsdMicros);
    const higher = Math.max(primary.priceUsdMicros, secondary.priceUsdMicros);
    const divergenceBps = Math.ceil(((higher - lower) * 10_000) / lower);
    if (divergenceBps > this.maxDivergenceBps) {
      throw new PolicyError(
        'price_inconsistent',
        'Independent price services disagree beyond the allowed threshold',
        503,
        true,
      );
    }
    return {
      priceUsdMicros: lower,
      observedAt: new Date(Math.min(primary.observedAt.getTime(), secondary.observedAt.getTime())),
      observations: [
        {
          feed: 'primary',
          priceUsdMicros: primary.priceUsdMicros,
          observedAt: primary.observedAt,
        },
        {
          feed: 'secondary',
          priceUsdMicros: secondary.priceUsdMicros,
          observedAt: secondary.observedAt,
        },
      ],
    };
  }
}

async function readLimitedJson(response: Response, limit: number): Promise<unknown> {
  if (!response.body)
    throw new PolicyError('price_invalid', 'Price response body is empty', 503, true);
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      throw new PolicyError(
        'price_invalid',
        'Price response body exceeds the allowed size',
        503,
        true,
      );
    }
    chunks.push(value);
  }
  const body = Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString('utf8');
  try {
    return JSON.parse(body);
  } catch {
    throw new PolicyError('price_invalid', 'Price response is not valid JSON', 503, true);
  }
}

export class FixedUsdPriceOracle implements UsdPriceOracle {
  constructor(readonly priceUsdMicros = 100_000_000) {}

  async solUsd(): Promise<UsdPrice> {
    const observedAt = new Date();
    return {
      priceUsdMicros: this.priceUsdMicros,
      observedAt,
      observations: [
        { feed: 'primary', priceUsdMicros: this.priceUsdMicros, observedAt },
        { feed: 'secondary', priceUsdMicros: this.priceUsdMicros, observedAt },
      ],
    };
  }
}

export class MockChainGateway implements ChainGateway {
  readonly settlements = new Map<string, SettlementFacts>();
  readonly states = new Map<string, TransactionState>();
  readonly applications = new Map<string, number>();
  readonly preparedOperations: ChainOperation[] = [];
  currentBlockHeight = 10;
  autoFinalize = true;
  throwAfterBroadcastOnce = false;
  refundRawAmount = 1_000_000_000n;
  escrowLamports = 100_000_000_000n;
  stateRentLamports = 2_000_000n;
  vaultRentLamports = 1_000_000n;
  guardRentLamports = 1_500_000n;
  now: () => number = Date.now;

  async readSettlement(signature: string): Promise<SettlementFacts> {
    const facts = this.settlements.get(signature);
    if (!facts)
      throw new PolicyError('settlement_not_found', 'Settlement transaction was not found', 422);
    return structuredClone(facts);
  }

  async prepare(operation: ChainOperation): Promise<PreparedTransaction> {
    this.preparedOperations.push(structuredClone(operation));
    const digest = createHash('sha256').update(JSON.stringify(operation)).digest();
    const signature = base58Encode(Buffer.concat([digest, digest]));
    const derived: Record<string, string> =
      operation.kind === 'escrow_reserve'
        ? {
            escrowAddress: base58Encode(
              createHash('sha256').update(`state:${operation.bountyDigest}`).digest(),
            ),
            vaultAddress: base58Encode(
              createHash('sha256').update(`vault:${operation.bountyDigest}`).digest(),
            ),
            guardAddress: base58Encode(
              createHash('sha256').update(`guard:${operation.bountyDigest}`).digest(),
            ),
            bountyDigest: operation.bountyDigest,
          }
        : {};
    return {
      signature,
      wireTransaction: Buffer.from(JSON.stringify(operation)).toString('base64'),
      lastValidBlockHeight: this.currentBlockHeight + 100,
      derived,
    };
  }

  async broadcast(prepared: PreparedTransaction): Promise<void> {
    if (!this.states.has(prepared.signature)) {
      this.applications.set(
        prepared.signature,
        (this.applications.get(prepared.signature) ?? 0) + 1,
      );
      this.states.set(prepared.signature, this.autoFinalize ? 'finalized' : 'submitted');
    }
    if (this.throwAfterBroadcastOnce) {
      this.throwAfterBroadcastOnce = false;
      throw new Error('connection closed after broadcast');
    }
  }

  async transactionState(signature: string): Promise<TransactionState> {
    return this.states.get(signature) ?? 'missing';
  }

  async blockHeight(): Promise<number> {
    return this.currentBlockHeight;
  }

  async unixTime(): Promise<number> {
    return Math.floor(this.now() / 1_000);
  }

  async capacity(): Promise<ChainCapacity> {
    return {
      refundRawAmount: this.refundRawAmount.toString(),
      escrowLamports: this.escrowLamports.toString(),
      stateRentLamports: this.stateRentLamports.toString(),
      vaultRentLamports: this.vaultRentLamports.toString(),
      guardRentLamports: this.guardRentLamports.toString(),
    };
  }

  async refundCapacity(): Promise<string> {
    return this.refundRawAmount.toString();
  }

  async health(): Promise<ChainHealthEvidence> {
    return {
      ...(await this.capacity()),
      rpcProviders: 2,
      escrowProgramId: '4'.repeat(32),
      escrowProgramDataSha256: 'a'.repeat(64),
      escrowProgramImmutable: true,
    };
  }
}

function tokenProgram(program: 'spl-token' | 'token-2022'): PublicKey {
  return program === 'token-2022' ? TOKEN_2022_PROGRAM_ID : TOKEN_PROGRAM_ID;
}

function decodeHash(value: string, name: string): Buffer {
  if (!/^[a-f0-9]{64}$/.test(value)) throw new Error(`${name} must be 32-byte lowercase hex`);
  return Buffer.from(value, 'hex');
}

function parsePositiveU64(value: string, name: string): bigint {
  if (!/^\d+$/.test(value)) throw new Error(`${name} is invalid`);
  const parsed = BigInt(value);
  if (parsed <= 0n || parsed > 0xffff_ffff_ffff_ffffn) throw new Error(`${name} is outside u64`);
  return parsed;
}

function parseI64(value: string, name: string): bigint {
  if (!/^-?\d+$/.test(value)) throw new Error(`${name} is invalid`);
  const parsed = BigInt(value);
  if (parsed < -0x8000_0000_0000_0000n || parsed > 0x7fff_ffff_ffff_ffffn) {
    throw new Error(`${name} is outside i64`);
  }
  return parsed;
}

function byte(value: number, name: string): number {
  if (!Number.isInteger(value) || value < 0 || value > 255) {
    throw new Error(`${name} is outside u8`);
  }
  return value;
}

function requiredValue(value: string | undefined, name: string): string {
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function parseKeypair(value: string): Keypair {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new Error('Signer private key must be a JSON byte array');
  }
  if (
    !Array.isArray(parsed) ||
    parsed.length !== 64 ||
    parsed.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)
  ) {
    throw new Error('Signer private key must contain exactly 64 bytes');
  }
  return Keypair.fromSecretKey(Uint8Array.from(parsed));
}

function base58Encode(bytes: Uint8Array): string {
  const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  let value = 0n;
  for (const byte of bytes) value = value * 256n + BigInt(byte);
  let encoded = '';
  while (value > 0n) {
    const remainder = Number(value % 58n);
    value /= 58n;
    encoded = alphabet[remainder] + encoded;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    encoded = `1${encoded}`;
  }
  return encoded || '1';
}
