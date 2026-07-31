import { generateKeyPairSync } from 'node:crypto';
import {
  existsSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  actorFor,
  base58Decode,
  base58Encode,
  buildLegacyMemoTransaction,
  canonicalJson,
  createDevnetExecutionEnvelope,
  createEnforcementWitness,
  createTrustDocuments,
  DEVNET_GENESIS_HASH,
  enforceW011,
  executeAuthorizedW009,
  parseLegacyTransaction,
  privateKeyFromSolanaKeypair,
  sha256Hex,
  signEvent,
  verifyEnforcementWitness,
  verifyRpcEvidence,
} from './enforcement-lib.mjs';
import {
  assertNoLegacyConsumptionAt,
  canonicalConsumptionFile,
  reserveDurablyAt,
} from './durable-consumption-store.mjs';

const BLOCKHASH = '11111111111111111111111111111111';
const PROPOSAL_BLOCKHASH = base58Encode(Buffer.alloc(32, 1));
const SOURCE_DATA = 'untrusted-data';
const CREATED_AT = '2026-07-31T00:00:00.000Z';
const EXPIRES_AT = '2026-08-07T00:00:00.000Z';
const temporaryDirectories = [];
const canonicalTestFiles = [];

function testDirectory() {
  const directory = mkdtempSync(join(tmpdir(), 'covenant-w009-store-'));
  temporaryDirectories.push(directory);
  return directory;
}

function trackCanonicalFile(bundle) {
  const target = canonicalConsumptionFile(
    bundle.w009.grant_consumption.event.consumption_key,
  );
  canonicalTestFiles.push(target);
  return target;
}

afterEach(() => {
  while (canonicalTestFiles.length) {
    rmSync(canonicalTestFiles.pop(), { force: true });
  }
  while (temporaryDirectories.length) {
    rmSync(temporaryDirectories.pop(), { recursive: true, force: true });
  }
});

function reservationEvidence(bundle, reservedAt = '2026-07-31T00:00:30.000Z') {
  const record = {
    schema: 'covenant.grant-consumption-reservation.v1',
    run_id: bundle.run_id,
    consumption_key: bundle.w009.grant_consumption.event.consumption_key,
    proposal_hash: bundle.w009.proposal.event.proposal_hash,
    reserved_at: reservedAt,
  };
  return {
    scheme: 'canonical_exclusive_fsync_file.v1',
    record,
    record_sha256: `sha256:${sha256Hex(Buffer.from(`${canonicalJson(record)}\n`))}`,
  };
}

function solanaKeypair() {
  const privateKey = generateKeyPairSync('ed25519').privateKey;
  const jwk = privateKey.export({ format: 'jwk' });
  return Buffer.concat([Buffer.from(jwk.d, 'base64url'), Buffer.from(jwk.x, 'base64url')]);
}

function fixture({
  withDevnet = false,
  createdAt = CREATED_AT,
  expiresAt = EXPIRES_AT,
} = {}) {
  const agentSecret = solanaKeypair();
  const keys = {
    authorityKey: generateKeyPairSync('ed25519').privateKey,
    agentKey: privateKeyFromSolanaKeypair(agentSecret),
    approverKey: generateKeyPairSync('ed25519').privateKey,
    enforcerKey: generateKeyPairSync('ed25519').privateKey,
    verifierKey: generateKeyPairSync('ed25519').privateKey,
  };
  const roles = {
    agent: actorFor('agent', keys.agentKey),
    approver: actorFor('approver', keys.approverKey),
    enforcer: actorFor('enforcer', keys.enforcerKey),
    verifier: actorFor('verifier', keys.verifierKey),
  };
  const documents = createTrustDocuments({
    runId: 'w009-w011-test-run',
    createdAt,
    expiresAt,
    authorityKey: keys.authorityKey,
    roles,
  });
  const trust = {
    authorityRoot: documents.authorityRoot,
    roleManifest: documents.roleManifest,
    expectedAuthorityPublicKeyB64u:
      documents.authorityRoot.payload.authority.public_key_b64u,
  };
  const sourceTransaction = buildLegacyMemoTransaction(solanaKeypair(), BLOCKHASH, SOURCE_DATA);
  const bundle = createEnforcementWitness({
    runId: 'w009-w011-test-run',
    createdAt,
    expiresAt,
    feePayer: base58Encode(agentSecret.subarray(32)),
    ...keys,
    ...trust,
    source: {
      transaction_signature: sourceTransaction.signature,
      slot: 467835173,
      block_time: 1780856859,
      confirmation_status: 'finalized',
      instruction_index: 0,
      program_id: 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr',
      data_utf8: SOURCE_DATA,
      data_hash: sha256Hex(Buffer.from(SOURCE_DATA)),
      wire_transaction_base64: sourceTransaction.wire.toString('base64'),
      proposal_recent_blockhash: PROPOSAL_BLOCKHASH,
    },
  });
  if (withDevnet) {
    const memo = bundle.w009.execution_plan.event.transaction.instructions[0].data;
    const transaction = buildLegacyMemoTransaction(agentSecret, BLOCKHASH, memo);
    bundle.w009.devnet_execution = createDevnetExecutionEnvelope({
      bundle,
      trust,
      enforcerKey: keys.enforcerKey,
      transaction,
      slot: 480000000,
      blockTime: Date.parse('2026-07-31T00:01:00.000Z') / 1_000,
      recordedAt: '2026-07-31T00:02:00.000Z',
      reservationEvidence: reservationEvidence(bundle),
    });
  }
  return { bundle, trust, keys, agentSecret, sourceTransaction };
}

const clone = (value) => JSON.parse(JSON.stringify(value));

describe('base58', () => {
  it('round-trips leading zeroes and long signatures', () => {
    const bytes = Buffer.concat([Buffer.alloc(3), Buffer.from([...Array(64).keys()])]);
    expect(base58Decode(base58Encode(bytes))).toEqual(bytes);
  });

  it('pins the all-zero Solana public key', () => {
    expect(base58Encode(Buffer.alloc(32))).toBe('1'.repeat(32));
    expect(base58Decode('1'.repeat(32))).toEqual(Buffer.alloc(32));
  });
});

describe('trusted enforcement witness', () => {
  it('derives W009 and W011 claims from root-pinned signed evidence', () => {
    const source = fixture();
    const result = verifyEnforcementWitness(source.bundle, source.trust);
    expect(result.trust).toEqual({
      authority_root: 'pinned',
      role_manifest: 'root_signed_and_hash_pinned',
    });
    expect(result.w009).toMatchObject({
      unauthorized_attempt: 'denied',
      scoped_approval: 'verified',
      capability_subject_is_solana_signer: true,
      signed_one_use_reservation_claim: 'verified',
      runtime_replay_guard: 'not_observable_from_static_bundle',
      durable_reservation_evidence: 'not_recorded',
      offline_wire_execution: 'not_recorded',
      live_rpc_confirmation: 'not_checked',
    });
    expect(result.w011).toMatchObject({
      concrete_transaction_bytes: 'verified',
      separately_keyed_refutation: 'verified',
      enforcer_denial: 'verified',
      signed_no_submit_outcome: 'verified',
      callback_behavior: 'not_observable_from_static_bundle',
    });
    expect(source.bundle).not.toHaveProperty('claims');
    expect(source.bundle).not.toHaveProperty('actors');
  });

  it('rejects a substituted authority root', () => {
    const source = fixture();
    const other = fixture();
    expect(() =>
      verifyEnforcementWitness(source.bundle, {
        ...source.trust,
        authorityRoot: other.trust.authorityRoot,
      }),
    ).toThrow('authority root does not match the verifier trust anchor');
  });

  it('rejects a tampered role manifest before trusting its actors', () => {
    const source = fixture();
    const trust = clone(source.trust);
    trust.roleManifest.payload.roles.verifier.public_key_b64u =
      source.trust.roleManifest.payload.roles.agent.public_key_b64u;
    expect(() => verifyEnforcementWitness(source.bundle, trust)).toThrow(
      'role manifest hash does not match authority policy',
    );
  });

  it('rejects a forged event signature', () => {
    const source = fixture();
    const forged = clone(source.bundle);
    forged.w009.denial.event.status = 'authorized';
    expect(() => verifyEnforcementWitness(forged, source.trust)).toThrow(
      'w009.denial signature is invalid',
    );
  });

  it('rejects a forward or missing causal parent', () => {
    const source = fixture();
    const broken = clone(source.bundle);
    broken.w011.derived_proposal.event.parent_ids = [`${broken.run_id}:does-not-exist`];
    broken.w011.derived_proposal = signEvent(
      broken.w011.derived_proposal.event,
      source.keys.agentKey,
      source.trust.roleManifest.payload.roles.agent,
    );
    expect(() => verifyEnforcementWitness(broken, source.trust)).toThrow(
      'w011.derived_proposal references a missing or forward causal parent',
    );
  });

  it('rejects events outside the authority-manifest window', () => {
    const source = fixture();
    const broken = clone(source.bundle);
    broken.w011.prevented_outcome.event.timestamp = '2026-08-08T00:00:00.000Z';
    broken.w011.prevented_outcome = signEvent(
      broken.w011.prevented_outcome.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(broken, source.trust)).toThrow(
      'w011.prevented_outcome falls outside trusted manifest validity',
    );
  });

  it('rejects a delegation added to the reference grant', () => {
    const source = fixture();
    const delegated = clone(source.bundle);
    delegated.w009.approval_grant.event.capability.delegation = {
      delegate_key_id: source.trust.roleManifest.payload.roles.verifier.key_id,
    };
    delegated.w009.approval_grant = signEvent(
      delegated.w009.approval_grant.event,
      source.keys.approverKey,
      source.trust.roleManifest.payload.roles.approver,
    );
    expect(() => verifyEnforcementWitness(delegated, source.trust)).toThrow(
      'W009 reference grant must not delegate signing',
    );
  });

  it('rejects a changed signed authorization digest from the Memo plan', () => {
    const source = fixture();
    const tampered = clone(source.bundle);
    tampered.w009.authorization.event.audit_context = 'changed';
    tampered.w009.authorization = signEvent(
      tampered.w009.authorization.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(tampered, source.trust)).toThrow(
      'W009 execution plan authorization digest is invalid',
    );
  });

  it('requires a finalized execution record when requested', () => {
    const source = fixture();
    expect(() =>
      verifyEnforcementWitness(source.bundle, {
        ...source.trust,
        requireDevnetRecord: true,
      }),
    ).toThrow('a finalized devnet execution record is required');
  });

  it('verifies an offline Solana transaction signed by the capability subject', () => {
    const source = fixture({ withDevnet: true });
    const result = verifyEnforcementWitness(source.bundle, {
      ...source.trust,
      requireDevnetRecord: true,
    });
    expect(result.w009.offline_wire_execution).toBe('verified');
    expect(result.w009.live_rpc_confirmation).toBe('not_checked');
  });

  it('rejects a re-signed execution record with a forged durable reservation digest', () => {
    const source = fixture({ withDevnet: true });
    const forged = clone(source.bundle);
    forged.w009.devnet_execution.event.durable_reservation.record_sha256 =
      `sha256:${'00'.repeat(32)}`;
    forged.w009.devnet_execution = signEvent(
      forged.w009.devnet_execution.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(forged, source.trust)).toThrow(
      'W009 durable reservation digest is invalid',
    );
  });

  it('rejects a transaction signer that is not the capability subject', () => {
    const source = fixture();
    const memo = source.bundle.w009.execution_plan.event.transaction.instructions[0].data;
    const transaction = buildLegacyMemoTransaction(solanaKeypair(), BLOCKHASH, memo);
    source.bundle.w009.devnet_execution = createDevnetExecutionEnvelope({
      bundle: source.bundle,
      trust: source.trust,
      enforcerKey: source.keys.enforcerKey,
      transaction,
      slot: 480000000,
      blockTime: Date.parse('2026-07-31T00:01:00.000Z') / 1_000,
      recordedAt: '2026-07-31T00:02:00.000Z',
      reservationEvidence: reservationEvidence(source.bundle),
    });
    expect(() => verifyEnforcementWitness(source.bundle, source.trust)).toThrow(
      'W009 transaction signer is not the capability subject',
    );
  });

  it('rejects a finalized record before authorization or after expiry', () => {
    const source = fixture({ withDevnet: true });
    const early = clone(source.bundle);
    early.w009.devnet_execution.event.block_time =
      Date.parse('2026-07-30T23:59:59.000Z') / 1_000;
    early.w009.devnet_execution = signEvent(
      early.w009.devnet_execution.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(early, source.trust)).toThrow(
      'W009 transaction landed before authorization',
    );

    const late = clone(source.bundle);
    late.w009.devnet_execution.event.block_time =
      Date.parse('2026-08-07T00:00:01.000Z') / 1_000;
    late.w009.devnet_execution.event.timestamp = '2026-08-07T00:00:00.000Z';
    late.w009.devnet_execution = signEvent(
      late.w009.devnet_execution.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(late, source.trust)).toThrow(
      'W009 transaction landed after grant expiry',
    );
  });

  it('rejects mutated W011 transaction bytes even when the proposal is re-signed', () => {
    const source = fixture();
    const broken = clone(source.bundle);
    const message = Buffer.from(broken.w011.derived_proposal.event.message_base64, 'base64');
    message[message.length - 1] ^= 1;
    broken.w011.derived_proposal.event.message_base64 = message.toString('base64');
    broken.w011.derived_proposal = signEvent(
      broken.w011.derived_proposal.event,
      source.keys.agentKey,
      source.trust.roleManifest.payload.roles.agent,
    );
    expect(() => verifyEnforcementWitness(broken, source.trust)).toThrow(
      'W011 proposed message data is invalid',
    );
  });

  it('rejects a separately signed false W011 causal path', () => {
    const source = fixture();
    const broken = clone(source.bundle);
    broken.w011.verifier_refutation.event.causal_path[0] = `${broken.run_id}:w009-proposal`;
    broken.w011.verifier_refutation = signEvent(
      broken.w011.verifier_refutation.event,
      source.keys.verifierKey,
      source.trust.roleManifest.payload.roles.verifier,
    );
    expect(() => verifyEnforcementWitness(broken, source.trust)).toThrow(
      'W011 refutation causal path is invalid',
    );
  });

  it('rejects a W011 denial or outcome that permits signing', () => {
    const source = fixture();
    const allowed = clone(source.bundle);
    allowed.w011.enforcer_denial.event.status = 'authorized';
    allowed.w011.enforcer_denial = signEvent(
      allowed.w011.enforcer_denial.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(allowed, source.trust)).toThrow(
      'W011 enforcer did not deny signing',
    );

    const submitted = clone(source.bundle);
    submitted.w011.prevented_outcome.event.submitted = true;
    submitted.w011.prevented_outcome = signEvent(
      submitted.w011.prevented_outcome.event,
      source.keys.enforcerKey,
      source.trust.roleManifest.payload.roles.enforcer,
    );
    expect(() => verifyEnforcementWitness(submitted, source.trust)).toThrow(
      'W011 outcome claims submission',
    );
  });
});

describe('reference enforcement callbacks', () => {
  it('rejects an expired grant before reserve or submit', async () => {
    const source = fixture({
      createdAt: '2026-07-01T00:00:00.000Z',
      expiresAt: '2026-07-02T00:00:00.000Z',
    });
    const reservationFile = trackCanonicalFile(source.bundle);
    const submit = vi.fn();
    await expect(
      executeAuthorizedW009({
        bundle: source.bundle,
        trust: source.trust,
        secretKey: source.agentSecret,
        recentBlockhash: BLOCKHASH,
        submit,
      }),
    ).rejects.toThrow('W009 grant expired before execution');
    expect(existsSync(reservationFile)).toBe(false);
    expect(submit).not.toHaveBeenCalled();
  });

  it('rechecks expiry after durable reservation and before signing', async () => {
    const source = fixture();
    const reservationFile = trackCanonicalFile(source.bundle);
    const submit = vi.fn();
    const expiresAt = Date.parse(EXPIRES_AT);
    const clock = vi
      .spyOn(Date, 'now')
      .mockReturnValueOnce(expiresAt - 1)
      .mockReturnValueOnce(expiresAt + 1);
    try {
      await expect(
        executeAuthorizedW009({
          bundle: source.bundle,
          trust: source.trust,
          secretKey: source.agentSecret,
          recentBlockhash: BLOCKHASH,
          submit,
        }),
      ).rejects.toThrow('W009 grant expired before execution');
      expect(existsSync(reservationFile)).toBe(true);
      expect(submit).not.toHaveBeenCalled();
    } finally {
      clock.mockRestore();
    }
  });

  it('rejects no-op stores and two caller-supplied alternate namespaces', async () => {
    const source = fixture();
    const submit = vi.fn();
    const firstDirectory = testDirectory();
    const secondDirectory = testDirectory();
    const input = {
      bundle: source.bundle,
      trust: source.trust,
      secretKey: source.agentSecret,
      recentBlockhash: BLOCKHASH,
      submit,
    };
    await expect(
      executeAuthorizedW009({
        ...input,
        grantUseStore: { reserve: async () => ({}) },
      }),
    ).rejects.toThrow('W009 execution state namespace is module-owned');
    await expect(
      executeAuthorizedW009({
        ...input,
        stateDirectory: firstDirectory,
      }),
    ).rejects.toThrow('W009 execution state namespace is module-owned');
    await expect(
      executeAuthorizedW009({
        ...input,
        stateDirectory: secondDirectory,
      }),
    ).rejects.toThrow('W009 execution state namespace is module-owned');
    expect(submit).not.toHaveBeenCalled();
  });

  it('consumes a W009 grant once and blocks replay before submit', async () => {
    const source = fixture();
    trackCanonicalFile(source.bundle);
    const submit = vi.fn(async ({ signature }) => signature);
    const input = {
      bundle: source.bundle,
      trust: source.trust,
      secretKey: source.agentSecret,
      recentBlockhash: BLOCKHASH,
      submit,
    };
    await executeAuthorizedW009(input);
    await expect(executeAuthorizedW009(input)).rejects.toThrow(
      'W009 grant replay blocked: durable consumption already exists',
    );
    expect(submit).toHaveBeenCalledTimes(1);
  });

  it('atomically persists and fsyncs the separately testable durable primitive', async () => {
    const source = fixture();
    const directory = testDirectory();
    const input = {
      runId: source.bundle.run_id,
      consumptionKey: source.bundle.w009.grant_consumption.event.consumption_key,
      proposalHash: source.bundle.w009.proposal.event.proposal_hash,
    };
    const first = await reserveDurablyAt(directory, input);
    expect(first.scheme).toBe(
      'canonical_exclusive_fsync_file.v1',
    );
    await expect(
      reserveDurablyAt(directory, input),
    ).rejects.toThrow('W009 grant replay blocked: durable consumption already exists');
    expect(readdirSync(directory)).toHaveLength(1);
  });

  it('blocks a grant already consumed by the fixed legacy journal', async () => {
    const source = fixture();
    const directory = testDirectory();
    const record = {
      run_id: source.bundle.run_id,
      consumption_key: source.bundle.w009.grant_consumption.event.consumption_key,
      reserved_at: new Date().toISOString(),
    };
    writeFileSync(
      join(directory, `${source.bundle.run_id}.consumed.json`),
      `${JSON.stringify(record)}\n`,
      { mode: 0o600 },
    );
    await expect(
      assertNoLegacyConsumptionAt(directory, {
        runId: record.run_id,
        consumptionKey: record.consumption_key,
      }),
    ).rejects.toThrow('W009 grant replay blocked: legacy durable consumption already exists');
  });

  it('never calls submit for the W011 refuted proposal', async () => {
    const source = fixture();
    const submit = vi.fn();
    const result = await enforceW011({
      bundle: source.bundle,
      trust: source.trust,
      submit,
    });
    expect(result).toMatchObject({ status: 'prevented', submit_callback_called: false });
    expect(submit).not.toHaveBeenCalled();
  });
});

describe('live RPC evidence adapter', () => {
  function rpcFixture(source, { w009BlockTimeDelta = 0 } = {}) {
    return vi.fn(async (method, params = []) => {
      if (method === 'getGenesisHash') return DEVNET_GENESIS_HASH;
      const signature = params[0];
      const w011 = source.bundle.w011.untrusted_input.event.source;
      const w009 = source.bundle.w009.devnet_execution.event;
      const record = signature === w011.transaction_signature ? w011 : w009;
      if (method === 'getTransaction') {
        return {
          slot: record.slot,
          blockTime:
            record.block_time +
            (record === w009 ? w009BlockTimeDelta : 0),
          meta: { err: null },
          transaction: [record.wire_transaction_base64, 'base64'],
        };
      }
      if (method === 'getSignatureStatuses') {
        return { value: [{ err: null, confirmationStatus: 'finalized' }] };
      }
      throw new Error(`unexpected RPC method ${method}`);
    });
  }

  it('marks exact finalized records as live-confirmed', async () => {
    const source = fixture({ withDevnet: true });
    const result = await verifyRpcEvidence(source.bundle, rpcFixture(source), source.trust);
    expect(result.w009.live_rpc_confirmation).toBe('verified');
    expect(result.w011.live_rpc_confirmation).toBe('verified');
  });

  it('rejects an exact block-time mismatch', async () => {
    const source = fixture({ withDevnet: true });
    await expect(
      verifyRpcEvidence(
        source.bundle,
        rpcFixture(source, { w009BlockTimeDelta: 1 }),
        source.trust,
      ),
    ).rejects.toThrow('W009 execution block time does not match RPC');
  });
});

describe('legacy Solana memo transaction', () => {
  const keypair = solanaKeypair();

  it('builds and verifies a one-signer Memo transaction', () => {
    const built = buildLegacyMemoTransaction(keypair, BLOCKHASH, 'covenant-test');
    const parsed = parseLegacyTransaction(built.wire);
    expect(parsed.signatures).toHaveLength(1);
    expect(parsed.instructions).toHaveLength(1);
    expect(parsed.instructions[0].data.toString('utf8')).toBe('covenant-test');
    expect(base58Encode(parsed.accountMeta[0].publicKey)).toBe(built.feePayer);
    expect(base58Encode(parsed.signatures[0])).toBe(built.signature);
  });

  it('rejects a one-byte message mutation', () => {
    const built = buildLegacyMemoTransaction(keypair, BLOCKHASH, 'covenant-test');
    const tampered = Buffer.from(built.wire);
    tampered[tampered.length - 1] ^= 1;
    expect(() => parseLegacyTransaction(tampered)).toThrow('Solana signature 0 is invalid');
  });
});
