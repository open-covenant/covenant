// @covenant/sap-bridge
//
// Opt-in bridge between Covenant and the Synapse Agent Protocol (SAP)
// on Solana. The on-chain surface is a thin wrapper over
// @oobe-protocol-labs/synapse-sap-sdk. The SDK, @solana/web3.js, and
// @coral-xyz/anchor are peer dependencies and are imported lazily, so
// consumers that only need `status()` / `resolveSynapseConfig()` — or
// that opt out of the on-chain path entirely — do not have to install
// them. Every network method gates on `config.enabled` first and
// throws `BridgeDisabledError`, which callers must treat as a soft
// no-op.

import { createHash } from 'node:crypto';
import { resolveSynapseConfig, type ResolvedSynapseConfig } from '@covenant/config/networks';
import type { PublicKey } from '@solana/web3.js';
// Type-only namespace imports. Erased at compile time — the runtime
// modules are pulled in lazily via createRequire (see loadSdk).
import type * as Web3 from '@solana/web3.js';
import type * as Anchor from '@coral-xyz/anchor';
import type * as SapSdk from '@oobe-protocol-labs/synapse-sap-sdk';

export { resolveSynapseConfig };
export type { ResolvedSynapseConfig };

export class BridgeDisabledError extends Error {
  constructor() {
    super('synapse bridge is disabled');
    this.name = 'BridgeDisabledError';
  }
}

export class BridgeSignerRequiredError extends Error {
  constructor(op: string) {
    super(`synapse bridge: ${op} requires a signer (set COVENANT_SAP_KEYPAIR)`);
    this.name = 'BridgeSignerRequiredError';
  }
}

// A loaded Solana keypair. A @solana/web3.js `Keypair` satisfies this
// shape; we keep the local type loose so the public surface does not
// import web3 eagerly.
export interface SapKeypair {
  publicKey: { toBase58(): string; toBuffer(): Buffer };
  secretKey: Uint8Array;
}

export interface CapabilityDescriptor {
  id: string;
  protocolId?: string | null;
  version?: string | null;
  description?: string | null;
}

// Covenant's simple pricing shape. Rich SAP pricing tiers (rate limits,
// token types, settlement modes) are mapped from budget rate cards in a
// follow-up; for now callers either omit pricing or pass SDK-native
// tiers through `pricingRaw`.
export interface PricingTier {
  id: string;
  priceUsdMicros: number;
  unit: string;
}

export interface AgentManifest {
  name: string;
  description?: string;
  capabilities: CapabilityDescriptor[];
  pricing: PricingTier[];
  protocols: string[];
  agentId?: string | null;
  agentUri?: string | null;
  x402Endpoint?: string | null;
  // Escape hatch: SDK-native PricingTier[] passed through untouched.
  pricingRaw?: unknown[];
}

export interface PublishedAgent {
  agentPda: string;
  signature: string;
}

export interface PeerRecord {
  agentPda: string;
  display: string;
  protocols: string[];
  reputationScore: number | null;
}

// Full agent projection used by reconcile / describe paths. Mirrors the
// fields publishAgent / updateAgent take, plus a few on-chain-only
// counters that help operators verify state.
export interface AgentDetail {
  agentPda: string;
  wallet: string;
  name: string;
  description: string;
  capabilities: CapabilityDescriptor[];
  pricing: unknown[];
  protocols: string[];
  agentId: string | null;
  agentUri: string | null;
  x402Endpoint: string | null;
  isActive: boolean;
  reputationScore: number | null;
}

// Audit-root anchoring. Only the 32-byte Merkle root and a small
// provenance envelope go on-chain — never the underlying audit-log
// contents. The root is written into a self-anchored SAP ledger (the
// daemon signs for its own agent); SAP rejects self-attestation by
// design, so the ledger module is the intended path for single-party
// audit trails.
export interface AuditRootAttestation {
  // 32-byte Merkle root as hex (with or without 0x). Goes on-chain as
  // the ledger entry's content_hash.
  rootHashHex: string;
  // Release provenance, packed into the ledger entry's `data` blob as a
  // compact JSON envelope. Identifiers only — never log contents.
  releaseTarget?: string;
  releaseSubject?: string;
  releaseScope?: string;
  recordedAt?: number;
  // Selects the audit ledger to append to (hashed into the session
  // seed). Defaults to a single canonical Covenant audit ledger so
  // successive roots accumulate in one append-only trail.
  ledgerLabel?: string;
}

export interface PublishedAuditRoot {
  // The ledger PDA the root was appended to. Stable across calls for a
  // given ledgerLabel, so external parties can follow the trail.
  ledgerPda: string;
  signature: string;
}

export interface SapBridgeOptions {
  config?: ResolvedSynapseConfig;
  signer?: SapKeypair;
}

export interface BridgeStatus {
  enabled: boolean;
  cluster: string;
  programId: string;
  rpcUrl: string;
  explorerUrl: string;
  hasSigner: boolean;
}

const AGENT_STATS_SEED = 'sap_stats';
const PRICING_MENU_SEED = 'sap_pricing';
// Ledger-flow PDA seeds, matching the deployed program's account
// constraints: vault = [sap_vault, agent], session = [sap_session,
// vault, session_hash], ledger = [sap_ledger, session].
const VAULT_SEED = 'sap_vault';
const SESSION_SEED = 'sap_session';
const LEDGER_SEED = 'sap_ledger';
// Canonical label for the daemon's audit ledger. Hashed into the
// session seed so the same ledger PDA is reused across roots.
const DEFAULT_AUDIT_LABEL = 'covenant.audit-root';
const AUDIT_VAULT_LABEL = 'covenant.audit-vault';

interface LoadedSdk {
  sdk: typeof SapSdk;
  web3: typeof Web3;
  anchor: typeof Anchor;
}

// Minimal shape of the Anchor methods builder, for the raw-program
// escape hatch (the ledger flow has no high-level SDK wrapper). The
// SDK types `program.methods.*` as possibly-undefined; we only need
// the two-step accountsPartial → instruction chain.
interface AnchorMethodBuilder {
  accountsPartial(accounts: Record<string, unknown>): AnchorMethodBuilder;
  instruction(): Promise<InstanceType<typeof Web3.TransactionInstruction>>;
}
// Named so access isn't widened to `| undefined` under
// noUncheckedIndexedAccess (an index signature would be).
interface LedgerProgramMethods {
  initVault(vaultNonce: number[]): AnchorMethodBuilder;
  openSession(sessionHash: number[]): AnchorMethodBuilder;
  initLedger(): AnchorMethodBuilder;
  writeLedger(data: Buffer, contentHash: number[]): AnchorMethodBuilder;
}

// Lazily pull in the optional on-chain dependencies. We load them
// through the CommonJS resolver rather than dynamic `import()` for two
// reasons: the SAP SDK's ESM build uses extensionless directory imports
// that native Node ESM rejects, and requiring all three from one
// resolver guarantees the SDK and this worker share a single
// @solana/web3.js / @coral-xyz/anchor instance (avoiding dual-package
// `instanceof` hazards). Throws a clear error — rather than a bare
// module-not-found — when an operator opts in without the peer deps.
async function loadSdk(): Promise<LoadedSdk> {
  try {
    const { createRequire } = await import('node:module');
    const require = createRequire(import.meta.url);
    return {
      sdk: require('@oobe-protocol-labs/synapse-sap-sdk') as typeof SapSdk,
      web3: require('@solana/web3.js') as typeof Web3,
      anchor: require('@coral-xyz/anchor') as typeof Anchor,
    };
  } catch (cause) {
    throw new Error(
      'synapse bridge: on-chain dependencies are not installed. Add ' +
        '@oobe-protocol-labs/synapse-sap-sdk, @solana/web3.js, and ' +
        '@coral-xyz/anchor to enable the bridge.',
      { cause },
    );
  }
}

// Sign and submit a VersionedTransaction. We do this directly against
// the connection rather than via SapClient.sendTransaction: that helper
// forwards `signers` into web3's `options` argument, which a
// VersionedTransaction send does not accept (it must be pre-signed),
// surfacing as a bare "Invalid arguments".
async function signAndSend(
  client: InstanceType<typeof SapSdk.SapClient>,
  tx: InstanceType<typeof Web3.VersionedTransaction>,
  signers: InstanceType<typeof Web3.Keypair>[],
): Promise<string> {
  tx.sign(signers);
  const signature = await client.connection.sendRawTransaction(tx.serialize(), {
    preflightCommitment: 'confirmed',
    maxRetries: 5,
  });
  await client.connection.confirmTransaction(signature, 'confirmed');
  return signature;
}

function hexToBytes32(hex: string): number[] {
  const clean = hex.startsWith('0x') ? hex.slice(2) : hex;
  if (clean.length !== 64 || /[^0-9a-fA-F]/.test(clean)) {
    throw new Error(
      `synapse bridge: audit root must be a 32-byte hex string, got ${clean.length / 2} bytes`,
    );
  }
  const out: number[] = [];
  for (let i = 0; i < 64; i += 2) {
    out.push(parseInt(clean.slice(i, i + 2), 16));
  }
  return out;
}

// Deterministic 32-byte seed from a label (sha256). Used for the
// session_hash and vault_nonce so the vault / session / ledger PDAs are
// stable across runs and roots append to one ledger.
function label32(label: string): number[] {
  return Array.from(createHash('sha256').update(label).digest());
}

function toSdkCapability(cap: CapabilityDescriptor) {
  // Anchor's coder camelCases IDL fields, so the Capability struct's
  // `protocol_id` is read as `protocolId` at encode time — emitting only
  // the snake key leaves protocolId undefined and the Option<string>
  // layout throws "indeterminate span". Emit both casings; the coder
  // reads the one its layout expects and ignores the other.
  const protocolId = cap.protocolId ?? null;
  return {
    id: cap.id,
    description: cap.description ?? null,
    protocol_id: protocolId,
    protocolId,
    version: cap.version ?? null,
  };
}

// Conservative defaults for SDK fields the simple Covenant
// PricingTier doesn't carry. Operators who need anything richer
// (custom rate limits, USDC, escrow / batched settlement, volume
// curves, SPL tokens) should pass SDK-native tiers through
// `manifest.pricingRaw` instead — that path is left untouched.
const DEFAULT_RATE_LIMIT = 1_000_000;
const DEFAULT_MAX_CALLS_PER_SESSION = 10_000;

// Map Covenant's simple PricingTier (id + USD-micros + unit, the
// shape that drops out of covenant-budget rate cards) into the
// SDK-native PricingTier the program expects. Native SOL,
// instant-settlement defaults; the `unit` is folded into the tier_id
// suffix so the on-chain account preserves the original semantics.
function toSdkPricingTier(tier: PricingTier, anchor: typeof Anchor) {
  return {
    tier_id: tier.unit ? `${tier.id}:${tier.unit}` : tier.id,
    price_per_call: new anchor.BN(tier.priceUsdMicros),
    min_price_per_call: null,
    max_price_per_call: null,
    rate_limit: DEFAULT_RATE_LIMIT,
    max_calls_per_session: DEFAULT_MAX_CALLS_PER_SESSION,
    burst_limit: null,
    token_type: { sol: {} },
    token_mint: null,
    token_decimals: null,
    settlement_mode: null,
    min_escrow_deposit: null,
    batch_interval_sec: null,
    volume_curve: null,
  };
}

// Choose the pricing payload sent to the SDK builder. `pricingRaw`
// wins when present — that's the escape hatch for callers building
// SDK-native tiers directly. Otherwise we map the simple
// Covenant shape through `toSdkPricingTier`.
function resolvePricing(manifest: AgentManifest, anchor: typeof Anchor): unknown[] {
  if (manifest.pricingRaw !== undefined) return manifest.pricingRaw;
  return manifest.pricing.map((t) => toSdkPricingTier(t, anchor));
}

export class SapBridge {
  readonly config: ResolvedSynapseConfig;
  private readonly signer?: SapKeypair;

  constructor(options: SapBridgeOptions = {}) {
    this.config = options.config ?? resolveSynapseConfig();
    this.signer = options.signer;
  }

  requireEnabled(): void {
    if (!this.config.enabled) {
      throw new BridgeDisabledError();
    }
  }

  private requireSigner(op: string): SapKeypair {
    if (!this.signer) {
      throw new BridgeSignerRequiredError(op);
    }
    return this.signer;
  }

  // Snapshot of the resolved bridge config. Safe to expose over an
  // operator surface — never returns secrets and does not touch the
  // network.
  status(): BridgeStatus {
    return {
      enabled: this.config.enabled,
      cluster: this.config.network.key,
      programId: this.config.programId,
      rpcUrl: this.config.rpcUrl,
      explorerUrl: this.config.explorerUrl,
      hasSigner: this.signer !== undefined,
    };
  }

  // Publish (register) this daemon's identity as a SAP agent account.
  async publishAgent(manifest: AgentManifest): Promise<PublishedAgent> {
    this.requireEnabled();
    const keypair = this.requireSigner('publishAgent');
    const { sdk, web3, anchor } = await loadSdk();

    const kp = keypair as unknown as InstanceType<typeof web3.Keypair>;
    const wallet = new anchor.Wallet(kp);
    const client = sdk.createSapClient(this.config.rpcUrl, wallet);
    const walletPk = new web3.PublicKey(keypair.publicKey.toBase58());

    const [agent] = sdk.Pdas.getAgentPDA(walletPk);
    // SDK 0.18.0's getAgentStatsPDA still seeds from the wallet, but the
    // deployed program enforces seeds = ["sap_stats", agent]. Derive it
    // ourselves so the on-chain ConstraintSeeds check passes.
    const programId = new web3.PublicKey(this.config.programId);
    const [agentStats] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from(AGENT_STATS_SEED), agent.toBuffer()],
      programId,
    );
    const [globalRegistry] = sdk.Pdas.getGlobalPDA();

    const ix = await client.agent.registerAgent({
      signer: kp,
      wallet: walletPk,
      agent,
      agentStats,
      globalRegistry,
      name: manifest.name,
      description: manifest.description ?? '',
      capabilities: manifest.capabilities.map(toSdkCapability) as never,
      pricing: resolvePricing(manifest, anchor) as never,
      protocols: manifest.protocols,
      agentId: manifest.agentId ?? null,
      agentUri: manifest.agentUri ?? null,
      x402Endpoint: manifest.x402Endpoint ?? null,
    });

    const tx = await client.buildTransaction([ix], walletPk);
    const signature = await signAndSend(client, tx, [kp]);
    return { agentPda: agent.toBase58(), signature };
  }

  // Anchor a Covenant audit root on-chain by appending it to a
  // self-anchored SAP ledger. The daemon signs for its own agent — SAP
  // rejects self-attestation by design (SelfAttestationNotAllowed), so
  // create_attestation is not usable for a single-party trail; the
  // ledger module (init → write → seal) is the intended path.
  //
  // Only the 32-byte root (as the entry's content_hash) and a small
  // provenance envelope (the entry's `data`) go on-chain — never the
  // underlying audit-log contents. The vault, session, and ledger are
  // created on first use and reused thereafter, so successive roots
  // accumulate in one append-only ledger keyed by `ledgerLabel`.
  async publishAuditRoot(attestation: AuditRootAttestation): Promise<PublishedAuditRoot> {
    this.requireEnabled();
    const keypair = this.requireSigner('publishAuditRoot');
    const { sdk, web3, anchor } = await loadSdk();

    const kp = keypair as unknown as InstanceType<typeof web3.Keypair>;
    const wallet = new anchor.Wallet(kp);
    const client = sdk.createSapClient(this.config.rpcUrl, wallet);
    const walletPk = new web3.PublicKey(keypair.publicKey.toBase58());
    const programId = new web3.PublicKey(this.config.programId);
    const systemProgram = web3.SystemProgram.programId;

    // Derive the ledger lineage directly from the documented seeds
    // rather than via SDK helpers — those have drifted from the program
    // (e.g. getAgentStatsPDA), and the on-chain ConstraintSeeds check is
    // the only contract that matters.
    const [agent] = sdk.Pdas.getAgentPDA(walletPk);
    const [globalRegistry] = sdk.Pdas.getGlobalPDA();
    const [vault] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from(VAULT_SEED), agent.toBuffer()],
      programId,
    );
    const sessionHash = label32(attestation.ledgerLabel ?? DEFAULT_AUDIT_LABEL);
    const [session] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from(SESSION_SEED), vault.toBuffer(), Buffer.from(sessionHash)],
      programId,
    );
    const [ledger] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from(LEDGER_SEED), session.toBuffer()],
      programId,
    );

    const contentHash = hexToBytes32(attestation.rootHashHex);
    // Provenance envelope — identifiers only, never contents.
    const data = Buffer.from(
      JSON.stringify({
        target: attestation.releaseTarget ?? null,
        subject: attestation.releaseSubject ?? null,
        scope: attestation.releaseScope ?? null,
        recordedAt: attestation.recordedAt ?? null,
      }),
      'utf8',
    );

    // The ledger module has no high-level SDK wrapper, so we build
    // against the raw Anchor program. Create the vault / session /
    // ledger on first use only; the write_ledger call always runs.
    const methods = client.program.methods as unknown as LedgerProgramMethods;
    const exists = async (pk: InstanceType<typeof web3.PublicKey>): Promise<boolean> =>
      (await client.connection.getAccountInfo(pk)) !== null;

    const ixs: InstanceType<typeof Web3.TransactionInstruction>[] = [];
    if (!(await exists(vault))) {
      ixs.push(
        await methods
          .initVault(label32(AUDIT_VAULT_LABEL))
          .accountsPartial({ wallet: walletPk, agent, vault, globalRegistry, systemProgram })
          .instruction(),
      );
    }
    if (!(await exists(session))) {
      ixs.push(
        await methods
          .openSession(sessionHash)
          .accountsPartial({ wallet: walletPk, agent, vault, session, systemProgram })
          .instruction(),
      );
    }
    if (!(await exists(ledger))) {
      ixs.push(
        await methods
          .initLedger()
          .accountsPartial({ wallet: walletPk, agent, vault, session, ledger, systemProgram })
          .instruction(),
      );
    }
    ixs.push(
      await methods
        .writeLedger(data, contentHash)
        .accountsPartial({ wallet: walletPk, session, vault, agent, ledger })
        .instruction(),
    );

    const tx = await client.buildTransaction(ixs, walletPk);
    const signature = await signAndSend(client, tx, [kp]);
    return { ledgerPda: ledger.toBase58(), signature };
  }

  // Update the daemon's on-chain agent account to reflect the local
  // manifest. The deployed program accepts each arg as optional — we
  // always pass non-null values so the manifest is the source of truth.
  async updateAgent(manifest: AgentManifest): Promise<PublishedAgent> {
    this.requireEnabled();
    const keypair = this.requireSigner('updateAgent');
    const { sdk, web3, anchor } = await loadSdk();

    const kp = keypair as unknown as InstanceType<typeof web3.Keypair>;
    const wallet = new anchor.Wallet(kp);
    const client = sdk.createSapClient(this.config.rpcUrl, wallet);
    const walletPk = new web3.PublicKey(keypair.publicKey.toBase58());
    const programId = new web3.PublicKey(this.config.programId);

    const [agent] = sdk.Pdas.getAgentPDA(walletPk);
    const [pricingMenu] = web3.PublicKey.findProgramAddressSync(
      [Buffer.from(PRICING_MENU_SEED), agent.toBuffer()],
      programId,
    );

    const ix = await client.agent.updateAgent({
      signer: kp,
      wallet: walletPk,
      agent,
      pricingMenu,
      name: manifest.name,
      description: manifest.description ?? '',
      capabilities: manifest.capabilities.map(toSdkCapability) as never,
      pricing: resolvePricing(manifest, anchor) as never,
      protocols: manifest.protocols,
      agentId: manifest.agentId ?? null,
      agentUri: manifest.agentUri ?? null,
      x402Endpoint: manifest.x402Endpoint ?? null,
    });

    const tx = await client.buildTransaction([ix], walletPk);
    const signature = await signAndSend(client, tx, [kp]);
    return { agentPda: agent.toBase58(), signature };
  }

  // Full agent projection — the read used by reconcile / diff paths
  // when a PeerRecord isn't enough.
  async describeAgent(pda: string): Promise<AgentDetail | null> {
    this.requireEnabled();
    const { sdk, web3 } = await loadSdk();
    const client = sdk.createSapClient(this.config.rpcUrl);
    const acct = await client.fetchAccount<RawAgentAccountFull>(
      'agentAccount',
      new web3.PublicKey(pda),
    );
    if (!acct) return null;
    return {
      agentPda: pda,
      wallet: acct.wallet.toBase58(),
      name: acct.name,
      description: acct.description ?? '',
      capabilities: (acct.capabilities ?? []).map((c) => ({
        id: c.id,
        protocolId: c.protocolId ?? c.protocol_id ?? null,
        version: c.version ?? null,
        description: c.description ?? null,
      })),
      pricing: acct.pricing ?? [],
      protocols: acct.protocols ?? [],
      agentId: acct.agentId ?? acct.agent_id ?? null,
      agentUri: acct.agentUri ?? acct.agent_uri ?? null,
      x402Endpoint: acct.x402Endpoint ?? acct.x402_endpoint ?? null,
      isActive: acct.isActive ?? acct.is_active ?? false,
      reputationScore: acct.reputationScore ?? null,
    };
  }

  // Resolve a single agent account by its on-chain PDA.
  async findAgentByPda(pda: string): Promise<PeerRecord | null> {
    this.requireEnabled();
    const { sdk, web3 } = await loadSdk();
    const client = sdk.createSapClient(this.config.rpcUrl);
    const acct = await client.fetchAccount<RawAgentAccount>(
      'agentAccount',
      new web3.PublicKey(pda),
    );
    if (!acct) return null;
    return mapAgent(pda, acct);
  }

  // Resolve peers advertising a given protocol via SAP's protocol
  // discovery index.
  async findAgentsByProtocol(protocol: string): Promise<PeerRecord[]> {
    this.requireEnabled();
    const { sdk, web3 } = await loadSdk();
    const client = sdk.createSapClient(this.config.rpcUrl);
    const [indexPda] = sdk.Pdas.getProtocolIndexPDA(sdk.Pdas.hashString(protocol));
    const index = await client.fetchAccount<RawProtocolIndex>('protocolIndex', indexPda);
    if (!index) return [];
    const out: PeerRecord[] = [];
    for (const agentPk of index.agents) {
      const acct = await client.fetchAccount<RawAgentAccount>('agentAccount', agentPk);
      if (acct) out.push(mapAgent(agentPk.toBase58(), acct));
    }
    return out;
  }
}

// Anchor decodes account fields to camelCase. We only read the few we
// surface as a PeerRecord.
interface RawAgentAccount {
  name: string;
  protocols: string[];
  reputationScore?: number;
}

interface RawProtocolIndex {
  agents: PublicKey[];
}

// Anchor decodes IDL types with mixed casing: snake_case for some
// fields (matching the Rust struct names) and camelCase for others.
// The deployed program returns a blend, so we accept both forms where
// it matters.
interface RawAgentAccountFull {
  wallet: PublicKey;
  name: string;
  description?: string;
  capabilities?: { id: string; description?: string | null; protocol_id?: string | null; protocolId?: string | null; version?: string | null }[];
  pricing?: unknown[];
  protocols?: string[];
  agent_id?: string | null;
  agentId?: string | null;
  agent_uri?: string | null;
  agentUri?: string | null;
  x402_endpoint?: string | null;
  x402Endpoint?: string | null;
  is_active?: boolean;
  isActive?: boolean;
  reputationScore?: number;
}

function mapAgent(pda: string, acct: RawAgentAccount): PeerRecord {
  return {
    agentPda: pda,
    display: acct.name,
    protocols: acct.protocols ?? [],
    reputationScore: acct.reputationScore ?? null,
  };
}
