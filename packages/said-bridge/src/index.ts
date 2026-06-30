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

function loadSdk(): SaidSdkLike {
  const require = createRequire(import.meta.url);
  try {
    const sdk = require('said-sdk');
    const web3 = require('@solana/web3.js');
    return { SAID: sdk.SAID, Keypair: web3.Keypair };
  } catch (cause) {
    throw new SaidSdkUnavailableError(cause);
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

  private client(): { client: SaidClientLike; wallet: SaidKeypairLike } {
    const { SAID, Keypair } = loadSdk();
    const signer = this.requireSigner('on-chain instruction');
    const wallet = Keypair.fromSecretKey(signer.secretKey);
    return {
      client: new SAID({ rpcUrl: this.config.rpcUrl, commitment: 'confirmed' }),
      wallet,
    };
  }

  async registerAgent(args: { metadataUri: string }): Promise<{
    agentPda: string;
    owner: string;
    signature: string;
  }> {
    this.requirePaid('register', 'register-agent');
    const { client, wallet } = this.client();
    const result = await client.registerAgent(wallet, args.metadataUri);
    return {
      agentPda: result.agentPDA,
      owner: wallet.publicKey.toBase58(),
      signature: result.txSignature,
    };
  }

  async getVerified(): Promise<{ signature: string; slot: number }> {
    this.requirePaid('verify', 'get-verified');
    const { client, wallet } = this.client();
    const result = await client.verifyAgent(wallet);
    return { signature: result.txSignature, slot: 0 };
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
