// @covenant/said-bridge. Thin wrapper over said-sdk. The SDK and
// @solana/web3.js are peer deps loaded lazily so consumers that only
// need status() or resolveSaidConfig() skip the dependency tree.

import { createRequire } from 'node:module';

import { resolveSaidConfig, type SaidConfig } from './config.js';

export { resolveSaidConfig, type SaidConfig };

export class BridgeDisabledError extends Error {
  constructor() {
    super('said bridge is disabled');
    this.name = 'BridgeDisabledError';
  }
}

export class BridgeSignerRequiredError extends Error {
  constructor(op: string) {
    super(`said bridge: ${op} requires a signer (set COVENANT_SAID_KEYPAIR)`);
    this.name = 'BridgeSignerRequiredError';
  }
}

export class BridgePaidGateClosedError extends Error {
  constructor(instruction: string) {
    super(`said bridge: paid gate for ${instruction} is closed`);
    this.name = 'BridgePaidGateClosedError';
  }
}

export class SaidSdkUnavailableError extends Error {
  constructor(cause: unknown) {
    super(
      'said bridge: said-sdk peer dependency is not installed. ' +
        'Run `pnpm add said-sdk @solana/web3.js` in the worker host.',
    );
    this.name = 'SaidSdkUnavailableError';
    (this as { cause?: unknown }).cause = cause;
  }
}

export interface BridgeOptions {
  config: SaidConfig;
  signer?: { publicKey: { toBase58(): string }; secretKey: Uint8Array };
}

interface SaidSdkLike {
  SAID: new (config?: { rpcUrl?: string; commitment?: string }) => SaidClientLike;
  Keypair: { fromSecretKey(secret: Uint8Array): SaidKeypairLike };
  Connection: new (rpcUrl: string, commitment?: string) => SaidConnectionLike;
}

interface SaidKeypairLike {
  publicKey: { toBase58(): string };
  secretKey: Uint8Array;
}

interface SaidClientLike {
  registerAgent(
    wallet: SaidKeypairLike,
    metadataUri: string,
    funder?: SaidKeypairLike,
  ): Promise<{ agentPDA: string; txSignature: string }>;
  verifyAgent(wallet: SaidKeypairLike): Promise<{ txSignature: string }>;
}

interface SaidConnectionLike {
  getTransaction(
    signature: string,
    options: { commitment: string; maxSupportedTransactionVersion: number },
  ): Promise<{ slot: number } | null>;
}

function loadSdk(): SaidSdkLike {
  const require = createRequire(import.meta.url);
  try {
    const sdk = require('said-sdk');
    const web3 = require('@solana/web3.js');
    return { SAID: sdk.SAID, Keypair: web3.Keypair, Connection: web3.Connection };
  } catch (cause) {
    throw new SaidSdkUnavailableError(cause);
  }
}

const DEFAULT_RPC_TIMEOUT_MS = 30_000;

function rpcTimeoutMs(): number {
  const raw = process.env.COVENANT_SAID_RPC_TIMEOUT_MS;
  const n = raw ? Number(raw) : NaN;
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_RPC_TIMEOUT_MS;
}

function withTimeout<T>(op: string, promise: Promise<T>): Promise<T> {
  const ms = rpcTimeoutMs();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new BridgeRpcTimeoutError(op, ms)),
      ms,
    );
    timer.unref?.();
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer) clearTimeout(timer);
  });
}

export class BridgeRpcTimeoutError extends Error {
  constructor(op: string, ms: number) {
    super(`said bridge: ${op} timed out after ${ms}ms`);
    this.name = 'BridgeRpcTimeoutError';
  }
}

export class SaidBridge {
  private readonly config: SaidConfig;
  private readonly signer?: BridgeOptions['signer'];

  constructor(opts: BridgeOptions) {
    this.config = opts.config;
    this.signer = opts.signer;
  }

  status(): SaidConfig {
    return this.config;
  }

  private requireEnabled(): void {
    if (!this.config.enabled) throw new BridgeDisabledError();
  }

  private requirePaid(instruction: keyof SaidConfig['paid'], op: string): void {
    this.requireEnabled();
    if (!this.config.paid[instruction]) throw new BridgePaidGateClosedError(op);
  }

  private requireSigner(op: string): NonNullable<BridgeOptions['signer']> {
    if (!this.signer) throw new BridgeSignerRequiredError(op);
    return this.signer;
  }

  private client(): {
    client: SaidClientLike;
    wallet: SaidKeypairLike;
    connection: SaidConnectionLike;
  } {
    const { SAID, Keypair, Connection } = loadSdk();
    const signer = this.requireSigner('on-chain instruction');
    const wallet = Keypair.fromSecretKey(signer.secretKey);
    return {
      client: new SAID({ rpcUrl: this.config.rpcUrl, commitment: 'confirmed' }),
      wallet,
      connection: new Connection(this.config.rpcUrl, 'confirmed'),
    };
  }

  async registerAgent(args: { metadataUri: string }): Promise<{
    agentPda: string;
    owner: string;
    signature: string;
  }> {
    this.requirePaid('register', 'register-agent');
    const { client, wallet } = this.client();
    const result = await withTimeout('register-agent', client.registerAgent(wallet, args.metadataUri));
    return {
      agentPda: result.agentPDA,
      owner: wallet.publicKey.toBase58(),
      signature: result.txSignature,
    };
  }

  async getVerified(): Promise<{ signature: string; slot: number }> {
    this.requirePaid('verify', 'get-verified');
    const { client, wallet, connection } = this.client();
    const result = await withTimeout('get-verified', client.verifyAgent(wallet));
    let slot = 0;
    try {
      const tx = await withTimeout(
        'get-verified:getTransaction',
        connection.getTransaction(result.txSignature, {
          commitment: 'confirmed',
          maxSupportedTransactionVersion: 0,
        }),
      );
      if (tx?.slot != null) slot = tx.slot;
    } catch {
      // verification landed; the slot fetch is best-effort
    }
    return { signature: result.txSignature, slot };
  }

  async submitAnchor(_args: {
    anchorIndex: number;
    startSeq: number;
    endSeq: number;
    merkleRootHex: string;
  }): Promise<{ txSig: string; slot: number }> {
    this.requirePaid('anchor', 'submit-anchor');
    throw new BridgeUnsupportedError(
      'submit-anchor',
      'said-sdk 0.3.4 does not expose submitAnchor. Re-enable once the SAID program publishes the anchor instruction in its public SDK.',
    );
  }

  async validateWork(_args: {
    agent: string;
    taskHashHex: string;
    passed: boolean;
    evidenceUri: string;
  }): Promise<{ validationPda: string; validator: string; signature: string }> {
    this.requirePaid('validateWork', 'validate-work');
    throw new BridgeUnsupportedError(
      'validate-work',
      'said-sdk 0.3.4 does not expose validateWork. Re-enable once the SAID program publishes the validation instruction in its public SDK.',
    );
  }
}

export class BridgeUnsupportedError extends Error {
  constructor(op: string, detail: string) {
    super(`said bridge: ${op} is not yet supported. ${detail}`);
    this.name = 'BridgeUnsupportedError';
  }
}
