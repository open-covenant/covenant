import { type Commitment, type Connection, PublicKey, type TransactionInstruction } from '@solana/web3.js';
import type { Hash32, SolanaAddress } from './accounts.js';
import {
  type AgentAccount,
  type ConfigAccount,
  type CreditAccount,
  type ReceiptBatchAccount,
  type StakePositionAccount,
  type TaskAccount,
  fetchAgent,
  fetchConfig,
  fetchCreditAccount,
  fetchReceiptBatch,
  fetchStakePosition,
  fetchTask,
} from './decode.js';
import {
  prepareAnchorReceiptBatchInstruction,
  prepareBuyCreditsInstruction,
  prepareCreateTaskInstruction,
  prepareRegisterAgentInstruction,
  prepareReleaseTaskInstruction,
  prepareStakeInstruction,
  type PreparedSolanaBundle,
} from './instructions.js';
import {
  deriveAgentPda,
  deriveConfigPda,
  deriveCreditsPda,
  deriveReceiptBatchPda,
  deriveStakePositionPda,
  deriveTaskPda,
} from './pda.js';
import { resolveSolanaNetwork } from './network.js';
import { toBase58, toPublicKey, type Address } from './pubkey.js';
import { toTransactionInstructions } from './serialize.js';
import type { CovenantSigner } from './signer.js';
import {
  buildTransaction,
  sendAndConfirmSignedTransaction,
  type BuildTransactionOptions,
  type SendOptions,
} from './transaction.js';

export interface CovenantClientOptions {
  connection: Connection;
  // Required for the write methods; reads work without one.
  signer?: CovenantSigner;
  // The CVNT SPL mint. If omitted, the client reads it from the on-chain config.
  covntMint?: Address;
  commitment?: Commitment;
  priorityFeeMicroLamports?: number;
  computeUnitLimit?: number;
}

export interface RegisterAgentParams {
  agentKey: Hash32;
  metadataHash: Hash32;
  capabilityHash: Hash32;
  operator?: Address;
}
export interface StakeParams {
  agentKey: Hash32;
  ownerCovntAccount: Address;
  stakeVault: Address;
  amountCovnt: string | bigint;
  lockUntil: string | bigint;
  owner?: Address;
  covntMint?: Address;
}
export interface BuyCreditsParams {
  ownerCovntAccount: Address;
  treasury: Address;
  amountCovnt: string | bigint;
  owner?: Address;
  covntMint?: Address;
}
export interface CreateTaskParams {
  agentKey: Hash32;
  taskId: Hash32;
  provider: Address;
  clientCovntAccount: Address;
  escrowVault: Address;
  amountCovnt: string | bigint;
  taskHash: Hash32;
  criteriaHash: Hash32;
  deadline: string | bigint;
  client?: Address;
  covntMint?: Address;
}
export interface ReleaseTaskParams {
  taskId: Hash32;
  escrowVault: Address;
  providerCovntAccount: Address;
  resultHash: Hash32;
  receiptHash: Hash32;
  client?: Address;
  covntMint?: Address;
}
export interface AnchorReceiptBatchParams {
  batchId: Hash32;
  merkleRoot: Hash32;
  receiptCount: number;
  authority?: Address;
}

// Reject a JS number (which silently loses precision above 2^53) and validate
// the integer format, per the string|bigint contract, before it reaches the wire.
function intString(value: string | bigint, label: string): string {
  const kind = typeof value;
  if (kind !== 'string' && kind !== 'bigint') {
    throw new Error(`${label} must be a string or bigint (a number risks precision loss), got ${kind}`);
  }
  const text = value.toString();
  if (!/^-?\d+$/.test(text)) {
    throw new Error(`${label} must be an integer, got ${JSON.stringify(value)}`);
  }
  return text;
}

// The high-level entry point: derives every PDA, fills the signer account, and
// runs each instruction build -> sign -> send -> confirm. Reads decode program
// state. Pass a signer for writes; reads and PDA derivation need only a connection.
export class CovenantClient {
  readonly connection: Connection;
  readonly programId: PublicKey;
  private readonly signer?: CovenantSigner;
  private readonly txOptions: BuildTransactionOptions & SendOptions;
  private readonly mintOverride?: Address;
  private mintPromise?: Promise<SolanaAddress>;

  constructor(options: CovenantClientOptions) {
    this.connection = options.connection;
    // Env-aware, matching the instruction builders (which resolve their program
    // from the network), so the PDA program and the instruction program agree.
    this.programId = toPublicKey(resolveSolanaNetwork().programId);
    this.signer = options.signer;
    this.mintOverride = options.covntMint;
    this.txOptions = {
      commitment: options.commitment ?? 'confirmed',
      computeUnitPriceMicroLamports: options.priorityFeeMicroLamports,
      computeUnitLimit: options.computeUnitLimit,
    };
  }

  configPda(): PublicKey {
    return deriveConfigPda(this.programId).address;
  }
  agentPda(agentKey: Hash32): PublicKey {
    return deriveAgentPda(agentKey, this.programId).address;
  }
  creditsPda(owner: Address): PublicKey {
    return deriveCreditsPda(owner, this.programId).address;
  }
  taskPda(taskId: Hash32): PublicKey {
    return deriveTaskPda(taskId, this.programId).address;
  }
  stakePositionPda(agentKey: Hash32, owner: Address): PublicKey {
    return deriveStakePositionPda(agentKey, owner, this.programId).address;
  }
  receiptBatchPda(batchId: Hash32): PublicKey {
    return deriveReceiptBatchPda(batchId, this.programId).address;
  }

  getConfig(): Promise<ConfigAccount | null> {
    return fetchConfig(this.connection, this.configPda());
  }
  getAgent(agentKey: Hash32): Promise<AgentAccount | null> {
    return fetchAgent(this.connection, this.agentPda(agentKey));
  }
  getCredits(owner: Address): Promise<CreditAccount | null> {
    return fetchCreditAccount(this.connection, this.creditsPda(owner));
  }
  getTask(taskId: Hash32): Promise<TaskAccount | null> {
    return fetchTask(this.connection, this.taskPda(taskId));
  }
  getStakePosition(agentKey: Hash32, owner: Address): Promise<StakePositionAccount | null> {
    return fetchStakePosition(this.connection, this.stakePositionPda(agentKey, owner));
  }
  getReceiptBatch(batchId: Hash32): Promise<ReceiptBatchAccount | null> {
    return fetchReceiptBatch(this.connection, this.receiptBatchPda(batchId));
  }

  // Writes build, sign, send, and confirm, then return the signature.
  async registerAgent(params: RegisterAgentParams): Promise<string> {
    const operator = params.operator ?? this.requireSigner().publicKey;
    return this.send(
      prepareRegisterAgentInstruction({
        configAccount: this.configPda().toBase58(),
        agentAccount: this.agentPda(params.agentKey).toBase58(),
        operator: toBase58(operator),
        agentKey: params.agentKey,
        metadataHash: params.metadataHash,
        capabilityHash: params.capabilityHash,
      }),
    );
  }

  async stake(params: StakeParams): Promise<string> {
    const owner = params.owner ?? this.requireSigner().publicKey;
    return this.send(
      prepareStakeInstruction({
        configAccount: this.configPda().toBase58(),
        agentAccount: this.agentPda(params.agentKey).toBase58(),
        positionAccount: this.stakePositionPda(params.agentKey, owner).toBase58(),
        owner: toBase58(owner),
        ownerCovntAccount: toBase58(params.ownerCovntAccount),
        stakeVault: toBase58(params.stakeVault),
        covntMint: await this.resolveMint(params.covntMint),
        amountCovnt: intString(params.amountCovnt, 'amountCovnt'),
        lockUntil: intString(params.lockUntil, 'lockUntil'),
      }),
    );
  }

  async buyCredits(params: BuyCreditsParams): Promise<string> {
    const owner = params.owner ?? this.requireSigner().publicKey;
    return this.send(
      prepareBuyCreditsInstruction({
        configAccount: this.configPda().toBase58(),
        creditAccount: this.creditsPda(owner).toBase58(),
        owner: toBase58(owner),
        ownerCovntAccount: toBase58(params.ownerCovntAccount),
        treasury: toBase58(params.treasury),
        covntMint: await this.resolveMint(params.covntMint),
        amountCovnt: intString(params.amountCovnt, 'amountCovnt'),
      }),
    );
  }

  async createTask(params: CreateTaskParams): Promise<string> {
    const client = params.client ?? this.requireSigner().publicKey;
    return this.send(
      prepareCreateTaskInstruction({
        configAccount: this.configPda().toBase58(),
        agentAccount: this.agentPda(params.agentKey).toBase58(),
        taskAccount: this.taskPda(params.taskId).toBase58(),
        client: toBase58(client),
        clientCovntAccount: toBase58(params.clientCovntAccount),
        escrowVault: toBase58(params.escrowVault),
        covntMint: await this.resolveMint(params.covntMint),
        provider: toBase58(params.provider),
        taskId: params.taskId,
        amountCovnt: intString(params.amountCovnt, 'amountCovnt'),
        taskHash: params.taskHash,
        criteriaHash: params.criteriaHash,
        deadline: intString(params.deadline, 'deadline'),
      }),
    );
  }

  async releaseTask(params: ReleaseTaskParams): Promise<string> {
    const client = params.client ?? this.requireSigner().publicKey;
    return this.send(
      prepareReleaseTaskInstruction({
        configAccount: this.configPda().toBase58(),
        taskAccount: this.taskPda(params.taskId).toBase58(),
        client: toBase58(client),
        escrowVault: toBase58(params.escrowVault),
        providerCovntAccount: toBase58(params.providerCovntAccount),
        covntMint: await this.resolveMint(params.covntMint),
        resultHash: params.resultHash,
        receiptHash: params.receiptHash,
      }),
    );
  }

  async anchorReceiptBatch(params: AnchorReceiptBatchParams): Promise<string> {
    const authority = params.authority ?? this.requireSigner().publicKey;
    return this.send(
      prepareAnchorReceiptBatchInstruction({
        configAccount: this.configPda().toBase58(),
        batchAccount: this.receiptBatchPda(params.batchId).toBase58(),
        authority: toBase58(authority),
        batchId: params.batchId,
        merkleRoot: params.merkleRoot,
        receiptCount: params.receiptCount,
      }),
    );
  }

  // Convert a prepared bundle to web3.js instructions for manual composition.
  toInstructions(bundle: PreparedSolanaBundle): TransactionInstruction[] {
    return toTransactionInstructions(bundle);
  }

  private requireSigner(): CovenantSigner {
    if (!this.signer) {
      throw new Error('this operation requires a signer; construct CovenantClient with { signer }');
    }
    return this.signer;
  }

  private async resolveMint(override?: Address): Promise<SolanaAddress> {
    const explicit = override ?? this.mintOverride;
    if (explicit) return toBase58(explicit);
    // Cache the in-flight promise so concurrent writes share one config read,
    // and drop it on failure so a transient RPC error is not cached forever.
    if (!this.mintPromise) {
      this.mintPromise = (async () => {
        const config = await this.getConfig();
        if (!config) {
          throw new Error('cannot resolve the CVNT mint: config not found on-chain and no covntMint was provided');
        }
        return config.covntMint;
      })();
      this.mintPromise.catch(() => {
        this.mintPromise = undefined;
      });
    }
    return this.mintPromise;
  }

  private async send(bundle: PreparedSolanaBundle): Promise<string> {
    const signer = this.requireSigner();
    const instructions = toTransactionInstructions(bundle);
    const tx = await buildTransaction(this.connection, signer.publicKey, instructions, this.txOptions);
    const signed = await signer.signTransaction(tx);
    return sendAndConfirmSignedTransaction(this.connection, signed, this.txOptions);
  }
}
