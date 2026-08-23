import { constants as fsConstants } from 'node:fs';
import { lstat, open, writeFile } from 'node:fs/promises';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  type AccountInfo,
  type TransactionInstruction,
} from '@solana/web3.js';
import { buildEscrowInstruction } from './chain.js';

const DEVNET_GENESIS_HASH = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1';
const UPGRADEABLE_LOADER_ID = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');
const STATE_MAGIC = Buffer.from('4d5a4b4553433100', 'hex');
const VAULT_MAGIC = Buffer.from('4d5a4b564c543100', 'hex');
const GUARD_MAGIC = Buffer.from('4d5a4b4752443100', 'hex');
const STATE_COMMITMENT_DOMAIN = Buffer.from('mizuki:escrow:state:v1');
const ZERO_PUBLIC_KEY = Buffer.alloc(32);
const STATE_BYTES = 236;
const VAULT_BYTES = 40;
const GUARD_BYTES = 108;
const PROGRAM_DATA_OFFSET = 45;
const MAX_ARTIFACT_BYTES = 16 * 1024 * 1024;
const FEE_RESERVE_LAMPORTS = 1_000_000n;
const MAX_WAIT_GRACE_SECONDS = 120;

export interface DevnetCanaryOptions {
  rpcUrlFile: string;
  programId: string;
  artifact: string;
  artifactSha256: string;
  artifactCommit: string;
  authorityKeypair: string;
  claimantKeypair: string;
  adversaryKeypair: string;
  output: string;
  amountLamports: number;
  expirySeconds: number;
  execute: boolean;
}

export interface ArtifactInspection {
  sha256: string;
  bytes: number;
  sbpfVersion: 2;
}

interface Scenario {
  bountyDigest: string;
  acceptanceHash: string;
  bindingEvidence: string;
  resolutionEvidence: string;
}

interface EscrowAddresses {
  state: PublicKey;
  vault: PublicKey;
  guard: PublicKey;
  stateBump: number;
  vaultBump: number;
  guardBump: number;
}

interface EscrowStateView {
  status: number;
  authority: PublicKey;
  claimant: PublicKey;
  bountyDigest: Buffer;
  amountLamports: bigint;
  createdAt: bigint;
  offerExpiresAt: bigint;
  claimExpiresAt: bigint;
  acceptanceHash: Buffer;
  bindingEvidence: Buffer;
  resolutionEvidence: Buffer;
}

interface EscrowExpectation {
  status: 1 | 2;
  authority: PublicKey;
  claimant?: PublicKey;
  scenario: Scenario;
  amountLamports: bigint;
  offerExpiresAt: bigint;
  claimExpiresAt?: bigint;
  stateLamports: bigint;
  vaultLamports: bigint;
  guardLamports: bigint;
}

interface ActiveEscrow {
  stateData: Buffer;
  stateLamports: bigint;
  vaultLamports: bigint;
}

interface TerminalExpectation {
  status: 3 | 4;
  authority: PublicKey;
  scenario: Scenario;
  activeState: Buffer;
  guardLamports: bigint;
  addresses: EscrowAddresses;
}

interface DeploymentEvidence {
  upgradeAuthorityPresent: boolean;
}

interface FlowReceipt {
  bountyDigest: string;
  signatures: Record<string, string>;
  assertions: Record<string, true>;
}

interface CanaryReceiptPayload {
  schemaVersion: 1;
  kind: 'mizuki_devnet_escrow_canary';
  status: 'dry_run_verified' | 'passed';
  createdAt: string;
  network: {
    cluster: 'devnet';
    genesisHash: string;
    commitment: 'finalized';
  };
  program: {
    id: string;
    loader: string;
    deployedArtifactMatch: true;
    upgradeAuthorityPresent: boolean;
  };
  artifact: {
    sha256: string;
    commit: string;
    bytes: number;
    sbpfVersion: 2;
  };
  canary: {
    amountLamports: string;
    expirySeconds: number;
    executionAuthorized: boolean;
    authorityCapacityVerified: true;
    adversaryCapacityVerified: true;
    distinctRoleKeysVerified: true;
    secretsRedacted: true;
    flowPlan: ['prefunded_release', 'bound_expiry_refund', 'unbound_expiry_refund'];
    flows?: {
      prefundedRelease: FlowReceipt;
      boundExpiryRefund: FlowReceipt;
      unboundExpiryRefund: FlowReceipt;
    };
  };
}

export interface CanaryReceipt extends CanaryReceiptPayload {
  payloadSha256: string;
}

export class DevnetCanaryError extends Error {
  constructor(readonly code: string) {
    super(code);
    this.name = 'DevnetCanaryError';
  }
}

export function parseDevnetCanaryArgs(args: string[]): DevnetCanaryOptions {
  const valueFlags = new Set([
    '--rpc-url-file',
    '--program-id',
    '--artifact',
    '--artifact-sha256',
    '--artifact-commit',
    '--authority-keypair',
    '--claimant-keypair',
    '--adversary-keypair',
    '--output',
    '--amount-lamports',
    '--expiry-seconds',
  ]);
  const values = new Map<string, string>();
  let execute = false;

  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (flag === '--execute') {
      if (execute) throw new DevnetCanaryError('duplicate_argument');
      execute = true;
      continue;
    }
    if (!flag || !valueFlags.has(flag)) {
      throw new DevnetCanaryError('invalid_argument');
    }
    if (values.has(flag)) throw new DevnetCanaryError('duplicate_argument');
    const value = args[index + 1];
    if (!value || value.startsWith('--')) {
      throw new DevnetCanaryError('missing_argument_value');
    }
    values.set(flag, value);
    index += 1;
  }

  const required = (flag: string): string => {
    const value = values.get(flag);
    if (!value) throw new DevnetCanaryError('missing_required_argument');
    return value;
  };
  const artifactSha256 = required('--artifact-sha256').toLowerCase();
  const artifactCommit = required('--artifact-commit').toLowerCase();
  if (!/^[a-f0-9]{64}$/.test(artifactSha256)) {
    throw new DevnetCanaryError('invalid_artifact_hash');
  }
  if (!/^[a-f0-9]{40,64}$/.test(artifactCommit)) {
    throw new DevnetCanaryError('invalid_artifact_commit');
  }

  const amountLamports = optionalInteger(values.get('--amount-lamports'), 1_000_000, 1, 10_000_000);
  const expirySeconds = optionalInteger(values.get('--expiry-seconds'), 90, 90, 300);

  return {
    rpcUrlFile: required('--rpc-url-file'),
    programId: required('--program-id'),
    artifact: required('--artifact'),
    artifactSha256,
    artifactCommit,
    authorityKeypair: required('--authority-keypair'),
    claimantKeypair: required('--claimant-keypair'),
    adversaryKeypair: required('--adversary-keypair'),
    output: required('--output'),
    amountLamports,
    expirySeconds,
    execute,
  };
}

export function validateDevnetRpcUrl(value: string): string {
  if (value.length === 0 || value.length > 8_192 || /[\r\n]/.test(value)) {
    throw new DevnetCanaryError('invalid_rpc_url');
  }
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new DevnetCanaryError('invalid_rpc_url');
  }
  if (
    url.protocol !== 'https:' ||
    url.username !== '' ||
    url.password !== '' ||
    /(?:^|[.\-_/])mainnet(?:-beta)?(?:$|[.\-_/])/i.test(value) ||
    /(?:^|\.)localhost$/i.test(url.hostname) ||
    url.hostname === '127.0.0.1' ||
    url.hostname === '::1'
  ) {
    throw new DevnetCanaryError('rpc_not_devnet_safe');
  }
  return url.toString();
}

export function inspectSbpfArtifact(data: Buffer, expectedSha256: string): ArtifactInspection {
  const sha256 = createHash('sha256').update(data).digest('hex');
  if (sha256 !== expectedSha256) throw new DevnetCanaryError('artifact_hash_mismatch');
  if (
    data.length < 64 ||
    !data.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) ||
    data[4] !== 2 ||
    data[5] !== 1 ||
    data.readUInt16LE(18) !== 247 ||
    data.readUInt32LE(48) !== 2
  ) {
    throw new DevnetCanaryError('artifact_not_sbpf_v2');
  }
  return { sha256, bytes: data.length, sbpfVersion: 2 };
}

export function inspectLoaderV3Deployment(
  programId: PublicKey,
  program: AccountInfo<Buffer> | null,
  programData: AccountInfo<Buffer> | null,
  artifact: Buffer,
  expectedSha256: string,
): DeploymentEvidence {
  if (
    !program ||
    !program.executable ||
    !program.owner.equals(UPGRADEABLE_LOADER_ID) ||
    program.data.length !== 36 ||
    program.data.readUInt32LE(0) !== 2
  ) {
    throw new DevnetCanaryError('invalid_program_deployment');
  }
  const programDataAddress = new PublicKey(program.data.subarray(4, 36));
  if (programDataAddress.equals(programId)) {
    throw new DevnetCanaryError('invalid_program_deployment');
  }
  if (
    !programData ||
    programData.executable ||
    !programData.owner.equals(UPGRADEABLE_LOADER_ID) ||
    programData.data.length <= PROGRAM_DATA_OFFSET ||
    programData.data.readUInt32LE(0) !== 3
  ) {
    throw new DevnetCanaryError('invalid_program_data');
  }
  const authorityOption = programData.data[12];
  if (authorityOption !== 0 && authorityOption !== 1) {
    throw new DevnetCanaryError('invalid_program_data');
  }
  const deployed = programData.data.subarray(PROGRAM_DATA_OFFSET);
  const deployedSha256 = createHash('sha256').update(deployed).digest('hex');
  if (deployedSha256 !== expectedSha256 || !deployed.equals(artifact)) {
    throw new DevnetCanaryError('deployed_artifact_mismatch');
  }
  return { upgradeAuthorityPresent: authorityOption === 1 };
}

export async function runDevnetCanary(options: DevnetCanaryOptions): Promise<CanaryReceipt> {
  const [rpcText, artifact, authority, claimant, adversary] = await Promise.all([
    readRestrictedText(options.rpcUrlFile, 8_192),
    readRegularFile(options.artifact, MAX_ARTIFACT_BYTES),
    readKeypair(options.authorityKeypair),
    readKeypair(options.claimantKeypair),
    readKeypair(options.adversaryKeypair),
  ]);
  const rpcUrl = validateDevnetRpcUrl(rpcText.trim());
  const inspection = inspectSbpfArtifact(artifact, options.artifactSha256);
  const programId = parseProgramId(options.programId);
  assertDistinctRoles(authority, claimant, adversary, programId);

  const connection = new Connection(rpcUrl, {
    commitment: 'finalized',
    confirmTransactionInitialTimeout: 60_000,
    disableRetryOnRateLimit: true,
  });
  const genesisHash = await connection.getGenesisHash();
  if (genesisHash !== DEVNET_GENESIS_HASH) {
    throw new DevnetCanaryError('rpc_genesis_not_devnet');
  }
  const deployment = await readDeployment(connection, programId, artifact, options.artifactSha256);

  const rents = {
    zero: BigInt(await connection.getMinimumBalanceForRentExemption(0, 'finalized')),
    state: BigInt(await connection.getMinimumBalanceForRentExemption(STATE_BYTES, 'finalized')),
    vault: BigInt(await connection.getMinimumBalanceForRentExemption(VAULT_BYTES, 'finalized')),
    guard: BigInt(await connection.getMinimumBalanceForRentExemption(GUARD_BYTES, 'finalized')),
  };
  const principal = BigInt(options.amountLamports);
  const authorityRequired =
    3n * (principal + rents.state + rents.vault + rents.guard) + FEE_RESERVE_LAMPORTS;
  const adversaryRequired = rents.zero * 2n + rents.vault + principal + 2n + FEE_RESERVE_LAMPORTS;
  await Promise.all([
    assertWallet(connection, authority.publicKey, authorityRequired),
    assertWallet(connection, claimant.publicKey, 0n),
    assertWallet(connection, adversary.publicKey, adversaryRequired),
  ]);

  const scenarios = {
    prefundedRelease: freshScenario(),
    boundExpiryRefund: freshScenario(),
    unboundExpiryRefund: freshScenario(),
  };
  const addresses = {
    prefundedRelease: deriveAddresses(programId, authority.publicKey, scenarios.prefundedRelease),
    boundExpiryRefund: deriveAddresses(programId, authority.publicKey, scenarios.boundExpiryRefund),
    unboundExpiryRefund: deriveAddresses(
      programId,
      authority.publicKey,
      scenarios.unboundExpiryRefund,
    ),
  };
  await assertFreshPdas(connection, Object.values(addresses));

  const base: CanaryReceiptPayload = {
    schemaVersion: 1,
    kind: 'mizuki_devnet_escrow_canary',
    status: options.execute ? 'passed' : 'dry_run_verified',
    createdAt: new Date().toISOString(),
    network: {
      cluster: 'devnet',
      genesisHash,
      commitment: 'finalized',
    },
    program: {
      id: programId.toBase58(),
      loader: UPGRADEABLE_LOADER_ID.toBase58(),
      deployedArtifactMatch: true,
      upgradeAuthorityPresent: deployment.upgradeAuthorityPresent,
    },
    artifact: {
      sha256: inspection.sha256,
      commit: options.artifactCommit,
      bytes: inspection.bytes,
      sbpfVersion: inspection.sbpfVersion,
    },
    canary: {
      amountLamports: principal.toString(),
      expirySeconds: options.expirySeconds,
      executionAuthorized: options.execute,
      authorityCapacityVerified: true,
      adversaryCapacityVerified: true,
      distinctRoleKeysVerified: true,
      secretsRedacted: true,
      flowPlan: ['prefunded_release', 'bound_expiry_refund', 'unbound_expiry_refund'],
    },
  };

  if (options.execute) {
    base.canary.flows = await executeCanaries({
      connection,
      programId,
      authority,
      claimant,
      adversary,
      principal,
      expirySeconds: options.expirySeconds,
      rents,
      scenarios,
      addresses,
    });
  }

  const receipt = sealReceipt(base);
  await writeReceipt(options.output, receipt);
  return receipt;
}

interface ExecutionContext {
  connection: Connection;
  programId: PublicKey;
  authority: Keypair;
  claimant: Keypair;
  adversary: Keypair;
  principal: bigint;
  expirySeconds: number;
  rents: { zero: bigint; state: bigint; vault: bigint; guard: bigint };
  scenarios: Record<'prefundedRelease' | 'boundExpiryRefund' | 'unboundExpiryRefund', Scenario>;
  addresses: Record<
    'prefundedRelease' | 'boundExpiryRefund' | 'unboundExpiryRefund',
    EscrowAddresses
  >;
}

async function executeCanaries(
  context: ExecutionContext,
): Promise<NonNullable<CanaryReceiptPayload['canary']['flows']>> {
  const {
    connection,
    programId,
    authority,
    claimant,
    adversary,
    principal,
    expirySeconds,
    rents,
    scenarios,
    addresses,
  } = context;
  const happyNow = await finalizedUnixTime(connection);
  const happyExpiry = happyNow + Math.max(expirySeconds * 4, 180);
  const happy = scenarios.prefundedRelease;
  const happyAddresses = addresses.prefundedRelease;
  const vaultPrefund = rents.vault + principal + 2n;

  const prefundSignature = await sendSuccessful(
    connection,
    adversary,
    [
      transfer(adversary.publicKey, happyAddresses.state, rents.zero),
      transfer(adversary.publicKey, happyAddresses.vault, vaultPrefund),
      transfer(adversary.publicKey, happyAddresses.guard, rents.zero),
    ],
    [adversary],
  );
  await assertPrefundedPdas(connection, happyAddresses, {
    state: rents.zero,
    vault: vaultPrefund,
    guard: rents.zero,
  });

  const happyFund = fundInstruction(programId, authority.publicKey, happy, principal, happyExpiry);
  const fundSignature = await sendSuccessful(connection, authority, [happyFund], [authority]);
  await assertActiveEscrow(connection, programId, happyAddresses, {
    status: 1,
    authority: authority.publicKey,
    scenario: happy,
    amountLamports: principal,
    offerExpiresAt: BigInt(happyExpiry),
    stateLamports: rents.state,
    vaultLamports: vaultPrefund,
    guardLamports: rents.guard,
  });

  const happyClaimExpiry = happyExpiry - 30;
  const bindSignature = await sendSuccessful(
    connection,
    authority,
    [bindInstruction(programId, authority.publicKey, happy, claimant.publicKey, happyClaimExpiry)],
    [authority],
  );
  const boundHappy = await assertActiveEscrow(connection, programId, happyAddresses, {
    status: 2,
    authority: authority.publicKey,
    claimant: claimant.publicKey,
    scenario: happy,
    amountLamports: principal,
    offerExpiresAt: BigInt(happyExpiry),
    claimExpiresAt: BigInt(happyClaimExpiry),
    stateLamports: rents.state,
    vaultLamports: vaultPrefund,
    guardLamports: rents.guard,
  });

  const wrongClaimantSignature = await sendExpectedFailure(
    connection,
    authority,
    [releaseInstruction(programId, authority.publicKey, happy, adversary.publicKey)],
    [authority],
  );
  await assertActiveEscrow(connection, programId, happyAddresses, {
    status: 2,
    authority: authority.publicKey,
    claimant: claimant.publicKey,
    scenario: happy,
    amountLamports: principal,
    offerExpiresAt: BigInt(happyExpiry),
    claimExpiresAt: BigInt(happyClaimExpiry),
    stateLamports: rents.state,
    vaultLamports: vaultPrefund,
    guardLamports: rents.guard,
  });

  const releaseSignature = await sendSuccessful(
    connection,
    authority,
    [releaseInstruction(programId, authority.publicKey, happy, claimant.publicKey)],
    [authority],
  );
  await assertTransactionDeltas(connection, releaseSignature, [
    { address: claimant.publicKey, delta: principal },
    {
      address: authority.publicKey,
      deltaPlusFee: boundHappy.stateLamports + boundHappy.vaultLamports - principal,
    },
  ]);
  const happyGuard = await assertTerminalEscrow(connection, programId, {
    status: 3,
    authority: authority.publicKey,
    scenario: happy,
    activeState: boundHappy.stateData,
    guardLamports: rents.guard,
    addresses: happyAddresses,
  });

  const replayReleaseSignature = await sendExpectedFailure(
    connection,
    authority,
    [releaseInstruction(programId, authority.publicKey, happy, claimant.publicKey)],
    [authority],
  );
  const replayFundSignature = await sendExpectedFailure(
    connection,
    authority,
    [fundInstruction(programId, authority.publicKey, happy, principal, happyExpiry)],
    [authority],
  );
  const replayGuard = await assertTerminalEscrow(connection, programId, {
    status: 3,
    authority: authority.publicKey,
    scenario: happy,
    activeState: boundHappy.stateData,
    guardLamports: rents.guard,
    addresses: happyAddresses,
  });
  if (!replayGuard.equals(happyGuard)) {
    throw new DevnetCanaryError('terminal_guard_changed');
  }

  const expiryNow = await finalizedUnixTime(connection);
  const sharedExpiry = expiryNow + expirySeconds;
  const boundOfferExpiry = sharedExpiry + expirySeconds;
  const bound = scenarios.boundExpiryRefund;
  const boundAddresses = addresses.boundExpiryRefund;
  const unbound = scenarios.unboundExpiryRefund;
  const unboundAddresses = addresses.unboundExpiryRefund;
  const normalVaultLamports = rents.vault + principal;

  const boundFundSignature = await sendSuccessful(
    connection,
    authority,
    [fundInstruction(programId, authority.publicKey, bound, principal, boundOfferExpiry)],
    [authority],
  );
  const boundBindSignature = await sendSuccessful(
    connection,
    authority,
    [bindInstruction(programId, authority.publicKey, bound, claimant.publicKey, sharedExpiry)],
    [authority],
  );
  const boundActive = await assertActiveEscrow(connection, programId, boundAddresses, {
    status: 2,
    authority: authority.publicKey,
    claimant: claimant.publicKey,
    scenario: bound,
    amountLamports: principal,
    offerExpiresAt: BigInt(boundOfferExpiry),
    claimExpiresAt: BigInt(sharedExpiry),
    stateLamports: rents.state,
    vaultLamports: normalVaultLamports,
    guardLamports: rents.guard,
  });

  const unboundFundSignature = await sendSuccessful(
    connection,
    authority,
    [fundInstruction(programId, authority.publicKey, unbound, principal, sharedExpiry)],
    [authority],
  );
  const unboundActive = await assertActiveEscrow(connection, programId, unboundAddresses, {
    status: 1,
    authority: authority.publicKey,
    scenario: unbound,
    amountLamports: principal,
    offerExpiresAt: BigInt(sharedExpiry),
    stateLamports: rents.state,
    vaultLamports: normalVaultLamports,
    guardLamports: rents.guard,
  });

  await waitForUnixTime(connection, sharedExpiry, expirySeconds + MAX_WAIT_GRACE_SECONDS);
  const expiredReleaseSignature = await sendExpectedFailure(
    connection,
    authority,
    [releaseInstruction(programId, authority.publicKey, bound, claimant.publicKey)],
    [authority],
  );
  await assertActiveEscrow(connection, programId, boundAddresses, {
    status: 2,
    authority: authority.publicKey,
    claimant: claimant.publicKey,
    scenario: bound,
    amountLamports: principal,
    offerExpiresAt: BigInt(boundOfferExpiry),
    claimExpiresAt: BigInt(sharedExpiry),
    stateLamports: rents.state,
    vaultLamports: normalVaultLamports,
    guardLamports: rents.guard,
  });

  const boundRefundSignature = await sendSuccessful(
    connection,
    authority,
    [refundInstruction(programId, authority.publicKey, bound)],
    [authority],
  );
  await assertTransactionDeltas(connection, boundRefundSignature, [
    {
      address: authority.publicKey,
      deltaPlusFee: boundActive.stateLamports + boundActive.vaultLamports,
    },
  ]);
  await assertTerminalEscrow(connection, programId, {
    status: 4,
    authority: authority.publicKey,
    scenario: bound,
    activeState: boundActive.stateData,
    guardLamports: rents.guard,
    addresses: boundAddresses,
  });

  const unboundRefundSignature = await sendSuccessful(
    connection,
    authority,
    [refundInstruction(programId, authority.publicKey, unbound)],
    [authority],
  );
  await assertTransactionDeltas(connection, unboundRefundSignature, [
    {
      address: authority.publicKey,
      deltaPlusFee: unboundActive.stateLamports + unboundActive.vaultLamports,
    },
  ]);
  await assertTerminalEscrow(connection, programId, {
    status: 4,
    authority: authority.publicKey,
    scenario: unbound,
    activeState: unboundActive.stateData,
    guardLamports: rents.guard,
    addresses: unboundAddresses,
  });

  return {
    prefundedRelease: {
      bountyDigest: happy.bountyDigest,
      signatures: {
        prefund: prefundSignature,
        fund: fundSignature,
        bind: bindSignature,
        wrongClaimantRejected: wrongClaimantSignature,
        release: releaseSignature,
        replayReleaseRejected: replayReleaseSignature,
        replayFundRejected: replayFundSignature,
      },
      assertions: {
        prefundedPdasAdopted: true,
        wrongClaimantRejected: true,
        exactPrincipalPaid: true,
        stateClosed: true,
        vaultClosed: true,
        terminalGuardVerified: true,
        releaseReplayRejected: true,
        fundReplayRejected: true,
      },
    },
    boundExpiryRefund: {
      bountyDigest: bound.bountyDigest,
      signatures: {
        fund: boundFundSignature,
        bind: boundBindSignature,
        expiredReleaseRejected: expiredReleaseSignature,
        refund: boundRefundSignature,
      },
      assertions: {
        expiredReleaseRejected: true,
        exactPrincipalRefunded: true,
        stateClosed: true,
        vaultClosed: true,
        terminalGuardVerified: true,
      },
    },
    unboundExpiryRefund: {
      bountyDigest: unbound.bountyDigest,
      signatures: {
        fund: unboundFundSignature,
        refund: unboundRefundSignature,
      },
      assertions: {
        exactPrincipalRefunded: true,
        stateClosed: true,
        vaultClosed: true,
        terminalGuardVerified: true,
      },
    },
  };
}

function fundInstruction(
  programId: PublicKey,
  authority: PublicKey,
  scenario: Scenario,
  amountLamports: bigint,
  expiresAt: number,
): TransactionInstruction {
  return buildEscrowInstruction(programId, authority, {
    kind: 'escrow_reserve',
    intentId: randomUUID(),
    bountyDigest: scenario.bountyDigest,
    amountLamports: amountLamports.toString(),
    expiresAtUnixSeconds: String(expiresAt),
    acceptanceHash: scenario.acceptanceHash,
  }).instruction;
}

function bindInstruction(
  programId: PublicKey,
  authority: PublicKey,
  scenario: Scenario,
  claimant: PublicKey,
  expiresAt: number,
): TransactionInstruction {
  return buildEscrowInstruction(programId, authority, {
    kind: 'escrow_bind',
    intentId: randomUUID(),
    bountyDigest: scenario.bountyDigest,
    claimantWallet: claimant.toBase58(),
    claimExpiresAtUnixSeconds: String(expiresAt),
    bindingEvidence: scenario.bindingEvidence,
  }).instruction;
}

function releaseInstruction(
  programId: PublicKey,
  authority: PublicKey,
  scenario: Scenario,
  claimant: PublicKey,
): TransactionInstruction {
  return buildEscrowInstruction(programId, authority, {
    kind: 'escrow_release',
    intentId: randomUUID(),
    bountyDigest: scenario.bountyDigest,
    claimantWallet: claimant.toBase58(),
    resolutionEvidence: scenario.resolutionEvidence,
  }).instruction;
}

function refundInstruction(
  programId: PublicKey,
  authority: PublicKey,
  scenario: Scenario,
): TransactionInstruction {
  return buildEscrowInstruction(programId, authority, {
    kind: 'escrow_refund',
    intentId: randomUUID(),
    bountyDigest: scenario.bountyDigest,
    resolutionEvidence: scenario.resolutionEvidence,
  }).instruction;
}

function transfer(from: PublicKey, to: PublicKey, lamports: bigint): TransactionInstruction {
  if (lamports > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DevnetCanaryError('unsafe_lamport_value');
  }
  return SystemProgram.transfer({ fromPubkey: from, toPubkey: to, lamports: Number(lamports) });
}

async function sendSuccessful(
  connection: Connection,
  payer: Keypair,
  instructions: TransactionInstruction[],
  signers: Keypair[],
): Promise<string> {
  const signed = await signTransaction(connection, payer, instructions, signers);
  let signature: string;
  try {
    signature = await connection.sendRawTransaction(signed.wire, {
      skipPreflight: false,
      maxRetries: 3,
    });
  } catch {
    throw new DevnetCanaryError('transaction_submission_failed');
  }
  const status = await confirmFinalized(connection, signature, signed.blockhash);
  if (status !== null) throw new DevnetCanaryError('transaction_failed');
  return signature;
}

async function sendExpectedFailure(
  connection: Connection,
  payer: Keypair,
  instructions: TransactionInstruction[],
  signers: Keypair[],
): Promise<string> {
  const signed = await signTransaction(connection, payer, instructions, signers);
  let signature: string;
  try {
    signature = await connection.sendRawTransaction(signed.wire, {
      skipPreflight: true,
      maxRetries: 3,
    });
  } catch {
    throw new DevnetCanaryError('expected_failure_not_submitted');
  }
  const status = await confirmFinalized(connection, signature, signed.blockhash);
  if (status === null) throw new DevnetCanaryError('expected_failure_succeeded');
  return signature;
}

async function signTransaction(
  connection: Connection,
  payer: Keypair,
  instructions: TransactionInstruction[],
  signers: Keypair[],
): Promise<{
  wire: Buffer;
  blockhash: { blockhash: string; lastValidBlockHeight: number };
}> {
  const blockhash = await connection.getLatestBlockhash('finalized');
  const transaction = new Transaction({
    feePayer: payer.publicKey,
    recentBlockhash: blockhash.blockhash,
  });
  transaction.add(...instructions);
  const unique = new Map<string, Keypair>();
  unique.set(payer.publicKey.toBase58(), payer);
  for (const signer of signers) unique.set(signer.publicKey.toBase58(), signer);
  transaction.sign(...unique.values());
  return { wire: transaction.serialize(), blockhash };
}

async function confirmFinalized(
  connection: Connection,
  signature: string,
  blockhash: { blockhash: string; lastValidBlockHeight: number },
): Promise<unknown | null> {
  let confirmation;
  try {
    confirmation = await connection.confirmTransaction({ signature, ...blockhash }, 'finalized');
  } catch {
    throw new DevnetCanaryError('transaction_confirmation_failed');
  }
  const status = await connection.getSignatureStatus(signature, {
    searchTransactionHistory: true,
  });
  if (!status.value || status.value.confirmationStatus !== 'finalized') {
    throw new DevnetCanaryError('transaction_not_finalized');
  }
  if (JSON.stringify(status.value.err) !== JSON.stringify(confirmation.value.err)) {
    throw new DevnetCanaryError('transaction_status_inconsistent');
  }
  return status.value.err;
}

async function assertTransactionDeltas(
  connection: Connection,
  signature: string,
  expectations: Array<
    { address: PublicKey; delta: bigint } | { address: PublicKey; deltaPlusFee: bigint }
  >,
): Promise<void> {
  const transaction = await connection.getParsedTransaction(signature, {
    commitment: 'finalized',
    maxSupportedTransactionVersion: 0,
  });
  if (!transaction?.meta || transaction.meta.err !== null) {
    throw new DevnetCanaryError('transaction_evidence_unavailable');
  }
  const keys = transaction.transaction.message.accountKeys.map(({ pubkey }) => pubkey);
  for (const expectation of expectations) {
    const index = keys.findIndex((key) => key.equals(expectation.address));
    if (index < 0) throw new DevnetCanaryError('transaction_balance_evidence_missing');
    const delta =
      BigInt(transaction.meta.postBalances[index]) - BigInt(transaction.meta.preBalances[index]);
    const expected =
      'delta' in expectation
        ? expectation.delta
        : expectation.deltaPlusFee - BigInt(transaction.meta.fee);
    if (delta !== expected) throw new DevnetCanaryError('unexpected_balance_delta');
  }
}

async function assertPrefundedPdas(
  connection: Connection,
  addresses: EscrowAddresses,
  expected: { state: bigint; vault: bigint; guard: bigint },
): Promise<void> {
  const accounts = await connection.getMultipleAccountsInfo(
    [addresses.state, addresses.vault, addresses.guard],
    'finalized',
  );
  for (const [index, account] of accounts.entries()) {
    if (
      !account ||
      account.executable ||
      !account.owner.equals(SystemProgram.programId) ||
      account.data.length !== 0
    ) {
      throw new DevnetCanaryError('prefund_account_invalid');
    }
    const expectedLamports = [expected.state, expected.vault, expected.guard][index];
    if (BigInt(account.lamports) !== expectedLamports) {
      throw new DevnetCanaryError('prefund_balance_invalid');
    }
  }
}

async function assertActiveEscrow(
  connection: Connection,
  programId: PublicKey,
  addresses: EscrowAddresses,
  expected: EscrowExpectation,
): Promise<ActiveEscrow> {
  const [stateAccount, vaultAccount, guardAccount] = await connection.getMultipleAccountsInfo(
    [addresses.state, addresses.vault, addresses.guard],
    'finalized',
  );
  assertProgramAccount(stateAccount, programId, STATE_BYTES, expected.stateLamports);
  assertProgramAccount(vaultAccount, programId, VAULT_BYTES, expected.vaultLamports);
  assertProgramAccount(guardAccount, programId, GUARD_BYTES, expected.guardLamports);
  const state = decodeEscrowState(stateAccount.data);
  const claimant = expected.claimant ?? new PublicKey(ZERO_PUBLIC_KEY);
  if (
    state.status !== expected.status ||
    stateAccount.data[10] !== addresses.stateBump ||
    stateAccount.data[11] !== addresses.vaultBump ||
    !state.authority.equals(expected.authority) ||
    !state.claimant.equals(claimant) ||
    state.bountyDigest.toString('hex') !== expected.scenario.bountyDigest ||
    state.amountLamports !== expected.amountLamports ||
    state.offerExpiresAt !== expected.offerExpiresAt ||
    state.claimExpiresAt !== (expected.claimExpiresAt ?? 0n) ||
    state.acceptanceHash.toString('hex') !== expected.scenario.acceptanceHash ||
    state.bindingEvidence.toString('hex') !==
      (expected.status === 2
        ? expected.scenario.bindingEvidence
        : ZERO_PUBLIC_KEY.toString('hex')) ||
    !state.resolutionEvidence.equals(ZERO_PUBLIC_KEY) ||
    state.createdAt >= state.offerExpiresAt
  ) {
    throw new DevnetCanaryError('active_state_mismatch');
  }
  if (
    !vaultAccount.data.subarray(0, 8).equals(VAULT_MAGIC) ||
    !vaultAccount.data.subarray(8, 40).equals(addresses.state.toBuffer())
  ) {
    throw new DevnetCanaryError('active_vault_mismatch');
  }
  assertGuard(
    guardAccount.data,
    expected.status,
    expected.authority,
    expected.scenario.bountyDigest,
    stateCommitment(stateAccount.data),
    addresses.guardBump,
  );
  return {
    stateData: Buffer.from(stateAccount.data),
    stateLamports: BigInt(stateAccount.lamports),
    vaultLamports: BigInt(vaultAccount.lamports),
  };
}

async function assertTerminalEscrow(
  connection: Connection,
  programId: PublicKey,
  expected: TerminalExpectation,
): Promise<Buffer> {
  const [stateAccount, vaultAccount, guardAccount] = await connection.getMultipleAccountsInfo(
    [expected.addresses.state, expected.addresses.vault, expected.addresses.guard],
    'finalized',
  );
  if (stateAccount !== null || vaultAccount !== null) {
    throw new DevnetCanaryError('terminal_accounts_not_closed');
  }
  assertProgramAccount(guardAccount, programId, GUARD_BYTES, expected.guardLamports);
  const terminalState = Buffer.from(expected.activeState);
  terminalState[9] = expected.status;
  Buffer.from(expected.scenario.resolutionEvidence, 'hex').copy(terminalState, 204);
  assertGuard(
    guardAccount.data,
    expected.status,
    expected.authority,
    expected.scenario.bountyDigest,
    stateCommitment(terminalState),
    expected.addresses.guardBump,
  );
  return Buffer.from(guardAccount.data);
}

function assertProgramAccount(
  account: AccountInfo<Buffer> | null,
  programId: PublicKey,
  bytes: number,
  lamports: bigint,
): asserts account is AccountInfo<Buffer> {
  if (
    !account ||
    account.executable ||
    !account.owner.equals(programId) ||
    account.data.length !== bytes ||
    BigInt(account.lamports) !== lamports
  ) {
    throw new DevnetCanaryError('program_account_mismatch');
  }
}

export function decodeEscrowState(data: Buffer): EscrowStateView {
  if (
    data.length !== STATE_BYTES ||
    !data.subarray(0, 8).equals(STATE_MAGIC) ||
    data[8] !== 1 ||
    ![1, 2, 3, 4].includes(data[9])
  ) {
    throw new DevnetCanaryError('invalid_escrow_state');
  }
  return {
    status: data[9],
    authority: new PublicKey(data.subarray(12, 44)),
    claimant: new PublicKey(data.subarray(44, 76)),
    bountyDigest: data.subarray(76, 108),
    amountLamports: data.readBigUInt64LE(108),
    createdAt: data.readBigInt64LE(116),
    offerExpiresAt: data.readBigInt64LE(124),
    claimExpiresAt: data.readBigInt64LE(132),
    acceptanceHash: data.subarray(140, 172),
    bindingEvidence: data.subarray(172, 204),
    resolutionEvidence: data.subarray(204, 236),
  };
}

function assertGuard(
  data: Buffer,
  status: number,
  authority: PublicKey,
  bountyDigest: string,
  commitment: Buffer,
  bump: number,
): void {
  if (
    data.length !== GUARD_BYTES ||
    !data.subarray(0, 8).equals(GUARD_MAGIC) ||
    data[8] !== 1 ||
    data[9] !== status ||
    data[10] !== bump ||
    data[11] !== 0 ||
    !data.subarray(12, 44).equals(authority.toBuffer()) ||
    data.subarray(44, 76).toString('hex') !== bountyDigest ||
    !data.subarray(76, 108).equals(commitment)
  ) {
    throw new DevnetCanaryError('guard_mismatch');
  }
}

function stateCommitment(state: Buffer): Buffer {
  return createHash('sha256').update(STATE_COMMITMENT_DOMAIN).update(state).digest();
}

function deriveAddresses(
  programId: PublicKey,
  authority: PublicKey,
  scenario: Scenario,
): EscrowAddresses {
  const digest = Buffer.from(scenario.bountyDigest, 'hex');
  const [state, stateBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-escrow'), authority.toBuffer(), digest],
    programId,
  );
  const [vault, vaultBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-vault'), state.toBuffer()],
    programId,
  );
  const [guard, guardBump] = PublicKey.findProgramAddressSync(
    [Buffer.from('mizuki-guard'), authority.toBuffer(), digest],
    programId,
  );
  return { state, vault, guard, stateBump, vaultBump, guardBump };
}

async function assertFreshPdas(
  connection: Connection,
  addressGroups: EscrowAddresses[],
): Promise<void> {
  const accounts = await connection.getMultipleAccountsInfo(
    addressGroups.flatMap(({ state, vault, guard }) => [state, vault, guard]),
    'finalized',
  );
  if (accounts.some((account) => account !== null)) {
    throw new DevnetCanaryError('bounty_not_fresh');
  }
}

async function assertWallet(
  connection: Connection,
  address: PublicKey,
  requiredLamports: bigint,
): Promise<void> {
  const account = await connection.getAccountInfo(address, 'finalized');
  if (
    account &&
    (account.executable ||
      !account.owner.equals(SystemProgram.programId) ||
      account.data.length !== 0)
  ) {
    throw new DevnetCanaryError('role_wallet_invalid');
  }
  if (BigInt(account?.lamports ?? 0) < requiredLamports) {
    throw new DevnetCanaryError('role_wallet_underfunded');
  }
}

async function readDeployment(
  connection: Connection,
  programId: PublicKey,
  artifact: Buffer,
  expectedSha256: string,
): Promise<DeploymentEvidence> {
  const program = await connection.getAccountInfo(programId, 'finalized');
  if (!program || program.data.length !== 36 || program.data.readUInt32LE(0) !== 2) {
    throw new DevnetCanaryError('invalid_program_deployment');
  }
  const programDataAddress = new PublicKey(program.data.subarray(4, 36));
  const programData = await connection.getAccountInfo(programDataAddress, 'finalized');
  return inspectLoaderV3Deployment(programId, program, programData, artifact, expectedSha256);
}

async function finalizedUnixTime(connection: Connection): Promise<number> {
  const slot = await connection.getSlot('finalized');
  const unixTime = await connection.getBlockTime(slot);
  if (unixTime === null || !Number.isSafeInteger(unixTime)) {
    throw new DevnetCanaryError('chain_time_unavailable');
  }
  return unixTime;
}

async function waitForUnixTime(
  connection: Connection,
  target: number,
  maxWaitSeconds: number,
): Promise<void> {
  const deadline = Date.now() + maxWaitSeconds * 1_000;
  while (Date.now() <= deadline) {
    if ((await finalizedUnixTime(connection)) >= target) return;
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new DevnetCanaryError('expiry_wait_timed_out');
}

function freshScenario(): Scenario {
  return {
    bountyDigest: createHash('sha256').update(`mizuki:bounty:v1:${randomUUID()}`).digest('hex'),
    acceptanceHash: nonzeroHash(),
    bindingEvidence: nonzeroHash(),
    resolutionEvidence: nonzeroHash(),
  };
}

function nonzeroHash(): string {
  let value = Buffer.alloc(32);
  while (value.equals(ZERO_PUBLIC_KEY)) value = randomBytes(32);
  return value.toString('hex');
}

function parseProgramId(value: string): PublicKey {
  let programId: PublicKey;
  try {
    programId = new PublicKey(value);
  } catch {
    throw new DevnetCanaryError('invalid_program_id');
  }
  if (
    programId.equals(SystemProgram.programId) ||
    programId.equals(UPGRADEABLE_LOADER_ID) ||
    programId.equals(PublicKey.default)
  ) {
    throw new DevnetCanaryError('invalid_program_id');
  }
  return programId;
}

function assertDistinctRoles(
  authority: Keypair,
  claimant: Keypair,
  adversary: Keypair,
  programId: PublicKey,
): void {
  const keys = [authority.publicKey, claimant.publicKey, adversary.publicKey, programId];
  if (new Set(keys.map((key) => key.toBase58())).size !== keys.length) {
    throw new DevnetCanaryError('role_keys_not_distinct');
  }
}

async function readKeypair(path: string): Promise<Keypair> {
  const data = await readRestrictedText(path, 4_096);
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    throw new DevnetCanaryError('invalid_keypair_file');
  }
  if (
    !Array.isArray(parsed) ||
    parsed.length !== 64 ||
    parsed.some((value) => !Number.isInteger(value) || value < 0 || value > 255)
  ) {
    throw new DevnetCanaryError('invalid_keypair_file');
  }
  try {
    return Keypair.fromSecretKey(Uint8Array.from(parsed));
  } catch {
    throw new DevnetCanaryError('invalid_keypair_file');
  }
}

async function readRestrictedText(path: string, maxBytes: number): Promise<string> {
  const stat = await safeStat(path, true, maxBytes);
  const handle = await safeOpen(path);
  try {
    const opened = await handle.stat();
    if (opened.dev !== stat.dev || opened.ino !== stat.ino) {
      throw new DevnetCanaryError('input_file_changed');
    }
    return await handle.readFile('utf8');
  } finally {
    await handle.close();
  }
}

async function readRegularFile(path: string, maxBytes: number): Promise<Buffer> {
  const stat = await safeStat(path, false, maxBytes);
  const handle = await safeOpen(path);
  try {
    const opened = await handle.stat();
    if (opened.dev !== stat.dev || opened.ino !== stat.ino) {
      throw new DevnetCanaryError('input_file_changed');
    }
    return await handle.readFile();
  } finally {
    await handle.close();
  }
}

async function safeStat(path: string, restricted: boolean, maxBytes: number) {
  let stat;
  try {
    stat = await lstat(path);
  } catch {
    throw new DevnetCanaryError('input_file_unavailable');
  }
  const currentUid = process.getuid?.();
  if (
    !stat.isFile() ||
    stat.isSymbolicLink() ||
    stat.size <= 0 ||
    stat.size > maxBytes ||
    (restricted && (stat.mode & 0o077) !== 0) ||
    (restricted && currentUid !== undefined && stat.uid !== currentUid)
  ) {
    throw new DevnetCanaryError('unsafe_input_file');
  }
  return stat;
}

async function safeOpen(path: string) {
  try {
    return await open(path, fsConstants.O_RDONLY | fsConstants.O_NOFOLLOW);
  } catch {
    throw new DevnetCanaryError('input_file_unavailable');
  }
}

function sealReceipt(payload: CanaryReceiptPayload): CanaryReceipt {
  const payloadSha256 = createHash('sha256').update(JSON.stringify(payload)).digest('hex');
  return { ...payload, payloadSha256 };
}

async function writeReceipt(path: string, receipt: CanaryReceipt): Promise<void> {
  try {
    await writeFile(path, `${JSON.stringify(receipt, null, 2)}\n`, {
      encoding: 'utf8',
      flag: 'wx',
      mode: 0o600,
    });
  } catch {
    throw new DevnetCanaryError('receipt_write_failed');
  }
}

function optionalInteger(
  value: string | undefined,
  fallback: number,
  minimum: number,
  maximum: number,
): number {
  if (value === undefined) return fallback;
  if (!/^\d+$/.test(value)) throw new DevnetCanaryError('invalid_numeric_argument');
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new DevnetCanaryError('numeric_argument_out_of_range');
  }
  return parsed;
}
