// @covenant/said-bridge
//
// Thin wrapper over the `said-sdk` npm package. The SDK and
// @solana/web3.js are peer dependencies and loaded lazily so consumers
// that only need `status()` / `resolveSaidConfig()` do not pay for the
// dependency tree.

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
  SAIDAgent: new (connection: unknown, wallet: unknown) => SaidAgentLike;
  Connection: new (rpcUrl: string, commitment?: string) => unknown;
}

interface SaidAgentLike {
  register(metadataUri: string): Promise<{ agentPda: string; signature: string }>;
  verify(): Promise<{ signature: string; slot?: number }>;
  submitAnchor(args: {
    anchorIndex: number | bigint;
    startSeq: number | bigint;
    endSeq: number | bigint;
    merkleRootHex: string;
  }): Promise<{ signature: string; slot?: number }>;
  validateWork(args: {
    agent: string;
    taskHashHex: string;
    passed: boolean;
    evidenceUri: string;
  }): Promise<{ validationPda: string; signature: string }>;
  sponsorRegister(args: {
    sponsoredOwner: string;
    metadataUri: string;
  }): Promise<{ agentPda: string; signature: string }>;
  sponsorVerify(args: { sponsoredOwner: string }): Promise<{ signature: string; slot?: number }>;
}

function loadSdk(): SaidSdkLike {
  const require = createRequire(import.meta.url);
  try {
    const sdk = require('said-sdk');
    const web3 = require('@solana/web3.js');
    return {
      SAIDAgent: sdk.SAIDAgent,
      Connection: web3.Connection,
    };
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

  private agent(): SaidAgentLike {
    const { SAIDAgent, Connection } = loadSdk();
    const connection = new Connection(this.config.rpcUrl, 'confirmed');
    return new SAIDAgent(connection, this.requireSigner('on-chain instruction'));
  }

  async registerAgent(args: { metadataUri: string }): Promise<{
    agentPda: string;
    owner: string;
    signature: string;
  }> {
    this.requirePaid('register', 'register-agent');
    const signer = this.requireSigner('register-agent');
    const agent = this.agent();
    const result = await agent.register(args.metadataUri);
    return {
      agentPda: result.agentPda,
      owner: signer.publicKey.toBase58(),
      signature: result.signature,
    };
  }

  async getVerified(): Promise<{ signature: string; slot: number }> {
    this.requirePaid('verify', 'get-verified');
    const agent = this.agent();
    const result = await agent.verify();
    return { signature: result.signature, slot: result.slot ?? 0 };
  }

  async submitAnchor(args: {
    anchorIndex: number;
    startSeq: number;
    endSeq: number;
    merkleRootHex: string;
  }): Promise<{ txSig: string; slot: number }> {
    this.requirePaid('anchor', 'submit-anchor');
    const agent = this.agent();
    const result = await agent.submitAnchor({
      anchorIndex: args.anchorIndex,
      startSeq: args.startSeq,
      endSeq: args.endSeq,
      merkleRootHex: args.merkleRootHex,
    });
    return { txSig: result.signature, slot: result.slot ?? 0 };
  }

  async validateWork(args: {
    agent: string;
    taskHashHex: string;
    passed: boolean;
    evidenceUri: string;
  }): Promise<{ validationPda: string; validator: string; signature: string }> {
    this.requirePaid('validateWork', 'validate-work');
    const signer = this.requireSigner('validate-work');
    const agent = this.agent();
    const result = await agent.validateWork(args);
    return {
      validationPda: result.validationPda,
      validator: signer.publicKey.toBase58(),
      signature: result.signature,
    };
  }

  async sponsorRegister(args: { sponsoredOwner: string; metadataUri: string }): Promise<{
    agentPda: string;
    owner: string;
    signature: string;
  }> {
    this.requirePaid('sponsor', 'sponsor-register');
    const agent = this.agent();
    const result = await agent.sponsorRegister(args);
    return {
      agentPda: result.agentPda,
      owner: args.sponsoredOwner,
      signature: result.signature,
    };
  }

  async sponsorVerify(args: {
    sponsoredOwner: string;
  }): Promise<{ signature: string; slot: number }> {
    this.requirePaid('sponsor', 'sponsor-verify');
    const agent = this.agent();
    const result = await agent.sponsorVerify(args);
    return { signature: result.signature, slot: result.slot ?? 0 };
  }
}
