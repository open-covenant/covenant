import {
  createHash,
  createPrivateKey,
  createPublicKey,
  sign as edSign,
  verify as edVerify,
} from 'node:crypto';
import { reserveCanonicalConsumption } from './durable-consumption-store.mjs';

export const ENFORCEMENT_SCHEMA = 'covenant.agent-safety-witness.v2';
export const AUTHORITY_ROOT_SCHEMA = 'covenant.agent-safety-authority-root.v1';
export const ROLE_MANIFEST_SCHEMA = 'covenant.agent-safety-role-manifest.v1';
export const EVENT_DOMAIN = 'covenant.agent-safety-event.v2';
export const ROLE_MANIFEST_DOMAIN = 'covenant.agent-safety-role-manifest.v1';
export const AUTHORITY_POLICY_DOMAIN = 'covenant.agent-safety-authority-policy.v1';
export const DEVNET_GENESIS_HASH = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
export const MEMO_PROGRAM_ID = 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr';

const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const BASE58_VALUES = new Map([...BASE58_ALPHABET].map((char, index) => [char, BigInt(index)]));
const ED25519_PKCS8_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const ED25519_SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

const assert = (condition, message) => {
  if (!condition) throw new Error(message);
};

export function canonicalJson(value) {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    assert(Number.isFinite(value), 'canonical JSON refuses non-finite numbers');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  assert(
    typeof value === 'object' && value !== undefined,
    'canonical JSON refuses unsupported values',
  );
  const entries = Object.keys(value)
    .sort()
    .map((key) => {
      assert(value[key] !== undefined, `canonical JSON refuses undefined at ${key}`);
      return `${JSON.stringify(key)}:${canonicalJson(value[key])}`;
    });
  return `{${entries.join(',')}}`;
}

export function sha256Hex(value) {
  return createHash('sha256').update(value).digest('hex');
}

export function hashObject(value) {
  return sha256Hex(Buffer.from(canonicalJson(value), 'utf8'));
}

export function base58Encode(bytes) {
  const input = Buffer.from(bytes);
  let value = input.length ? BigInt(`0x${input.toString('hex') || '0'}`) : 0n;
  let encoded = '';
  while (value > 0n) {
    encoded = BASE58_ALPHABET[Number(value % 58n)] + encoded;
    value /= 58n;
  }
  let leadingZeroes = 0;
  while (leadingZeroes < input.length && input[leadingZeroes] === 0) leadingZeroes += 1;
  return '1'.repeat(leadingZeroes) + encoded;
}

export function base58Decode(value) {
  assert(typeof value === 'string' && value.length > 0, 'base58 value must be a non-empty string');
  let decoded = 0n;
  for (const char of value) {
    const digit = BASE58_VALUES.get(char);
    assert(digit !== undefined, `invalid base58 character: ${char}`);
    decoded = decoded * 58n + digit;
  }
  let body = Buffer.alloc(0);
  if (decoded > 0n) {
    let hex = decoded.toString(16);
    if (hex.length % 2) hex = `0${hex}`;
    body = Buffer.from(hex, 'hex');
  }
  let leadingZeroes = 0;
  while (leadingZeroes < value.length && value[leadingZeroes] === '1') leadingZeroes += 1;
  return Buffer.concat([Buffer.alloc(leadingZeroes), body]);
}

function rawPublicKey(key) {
  const publicKey = key.type === 'private' ? createPublicKey(key) : key;
  return Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url');
}

function publicKeyFromActor(actor, label) {
  const publicBytes = Buffer.from(actor?.public_key_b64u || '', 'base64url');
  assert(publicBytes.length === 32, `${label} public key must be 32 bytes`);
  assert(actor.algorithm === 'ed25519', `${label} algorithm is invalid`);
  assert(
    actor.key_id === `sha256:${sha256Hex(publicBytes)}`,
    `${label} key id does not match public key`,
  );
  return createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, publicBytes]),
    format: 'der',
    type: 'spki',
  });
}

export function actorFor(role, key) {
  const publicKey = rawPublicKey(key);
  return {
    role,
    algorithm: 'ed25519',
    key_id: `sha256:${sha256Hex(publicKey)}`,
    public_key_b64u: publicKey.toString('base64url'),
  };
}

function signPayload(payload, domain, privateKey, actor) {
  const message = Buffer.from(`${domain}\n${canonicalJson(payload)}`, 'utf8');
  return {
    payload,
    signature: {
      algorithm: 'ed25519',
      domain,
      signer_role: actor.role,
      key_id: actor.key_id,
      value_b64u: edSign(null, message, privateKey).toString('base64url'),
    },
  };
}

function verifySignedPayload(envelope, actor, domain, label) {
  assert(envelope && typeof envelope === 'object', `${label} must be signed`);
  assert(envelope.signature?.algorithm === 'ed25519', `${label} signature algorithm is invalid`);
  assert(envelope.signature?.domain === domain, `${label} signature domain is invalid`);
  assert(envelope.signature?.signer_role === actor.role, `${label} signer role is invalid`);
  assert(envelope.signature?.key_id === actor.key_id, `${label} signer key is invalid`);
  const publicKey = publicKeyFromActor(actor, `${label} signer`);
  const signature = Buffer.from(envelope.signature.value_b64u || '', 'base64url');
  const message = Buffer.from(`${domain}\n${canonicalJson(envelope.payload)}`, 'utf8');
  assert(
    signature.length === 64 && edVerify(null, message, publicKey, signature),
    `${label} signature is invalid`,
  );
}

export function signEvent(eventValue, privateKey, actor) {
  const signed = signPayload(eventValue, EVENT_DOMAIN, privateKey, actor);
  return { event: signed.payload, signature: signed.signature };
}

function verifyEnvelope(envelope, actor, label) {
  verifySignedPayload(
    { payload: envelope?.event, signature: envelope?.signature },
    actor,
    EVENT_DOMAIN,
    label,
  );
}

export function createTrustDocuments({
  runId,
  createdAt,
  expiresAt,
  authorityKey,
  roles,
}) {
  const authority = actorFor('authority_root', authorityKey);
  const roleManifest = signPayload(
    {
      schema: ROLE_MANIFEST_SCHEMA,
      run_id: runId,
      issued_at: createdAt,
      expires_at: expiresAt,
      evidence_mode: 'standalone_reference_harness',
      roles,
    },
    ROLE_MANIFEST_DOMAIN,
    authorityKey,
    authority,
  );
  const roleManifestSha256 = sha256Hex(Buffer.from(canonicalJson(roleManifest), 'utf8'));
  const authorityRoot = signPayload(
    {
      schema: AUTHORITY_ROOT_SCHEMA,
      authority,
      witness_schema: ENFORCEMENT_SCHEMA,
      run_id: runId,
      issued_at: createdAt,
      expires_at: expiresAt,
      role_manifest_sha256: `sha256:${roleManifestSha256}`,
    },
    AUTHORITY_POLICY_DOMAIN,
    authorityKey,
    authority,
  );
  return { authorityRoot, roleManifest, roleManifestSha256 };
}

export function verifyTrustDocuments(
  authorityRoot,
  roleManifest,
  expectedAuthorityPublicKeyB64u,
) {
  assert(
    typeof expectedAuthorityPublicKeyB64u === 'string' &&
      Buffer.from(expectedAuthorityPublicKeyB64u, 'base64url').length === 32,
    'expected authority root public key is not configured',
  );
  const policy = authorityRoot?.payload;
  assert(policy?.schema === AUTHORITY_ROOT_SCHEMA, 'authority root schema is invalid');
  assert(
    policy.authority?.public_key_b64u === expectedAuthorityPublicKeyB64u,
    'authority root does not match the verifier trust anchor',
  );
  assert(policy.authority?.role === 'authority_root', 'authority root role is invalid');
  verifySignedPayload(
    authorityRoot,
    policy.authority,
    AUTHORITY_POLICY_DOMAIN,
    'authority root policy',
  );
  assert(policy.witness_schema === ENFORCEMENT_SCHEMA, 'authority policy witness schema is invalid');

  const manifestHash = `sha256:${sha256Hex(Buffer.from(canonicalJson(roleManifest), 'utf8'))}`;
  assert(
    policy.role_manifest_sha256 === manifestHash,
    'role manifest hash does not match authority policy',
  );
  verifySignedPayload(
    roleManifest,
    policy.authority,
    ROLE_MANIFEST_DOMAIN,
    'role manifest',
  );
  const manifest = roleManifest.payload;
  assert(manifest?.schema === ROLE_MANIFEST_SCHEMA, 'role manifest schema is invalid');
  assert(manifest.run_id === policy.run_id, 'role manifest run_id differs from authority policy');
  assert(
    manifest.issued_at === policy.issued_at && manifest.expires_at === policy.expires_at,
    'role manifest validity differs from authority policy',
  );
  assert(
    manifest.evidence_mode === 'standalone_reference_harness',
    'role manifest evidence mode is invalid',
  );
  const roles = manifest.roles || {};
  for (const role of ['agent', 'approver', 'enforcer', 'verifier']) {
    assert(roles[role]?.role === role, `trusted ${role} role is missing`);
    publicKeyFromActor(roles[role], `trusted ${role}`);
  }
  assert(
    new Set(Object.values(roles).map((actor) => actor.key_id)).size === 4,
    'trusted role keys are not distinct',
  );
  assert(
    Number.isFinite(Date.parse(manifest.issued_at)) &&
      Date.parse(manifest.issued_at) < Date.parse(manifest.expires_at),
    'role manifest validity window is invalid',
  );
  return { policy, manifest, roles, manifestHash };
}

function instructionDescriptor(programId, accounts, data) {
  const normalizedAccounts = accounts.map((account) => ({
    pubkey: account.pubkey,
    is_signer: account.is_signer,
    is_writable: account.is_writable,
  }));
  const normalized = {
    program_id: programId,
    accounts: normalizedAccounts,
    data_encoding: 'utf8',
    data,
  };
  return {
    normalized,
    program_hash: sha256Hex(Buffer.from(programId, 'utf8')),
    accounts_hash: hashObject(normalizedAccounts),
    data_hash: sha256Hex(Buffer.from(data, 'utf8')),
    instruction_hash: hashObject(normalized),
  };
}

function proposalScope({ feePayer, expiresAt, nonce, runId }) {
  return {
    cluster: 'devnet',
    fee_payer: feePayer,
    program_id: MEMO_PROGRAM_ID,
    program_hash: sha256Hex(Buffer.from(MEMO_PROGRAM_ID, 'utf8')),
    accounts: [],
    accounts_hash: hashObject([]),
    action: 'memo.write',
    data_commitment_scheme: 'covenant-safety-memo.v2',
    data_intent_hash: hashObject({ run_id: runId, nonce }),
    expires_at: expiresAt,
    nonce,
    max_uses: 1,
  };
}

function event(runId, id, parentIds, timestamp, type, fields = {}) {
  return {
    id: `${runId}:${id}`,
    run_id: runId,
    parent_ids: parentIds.map((parent) => `${runId}:${parent}`),
    timestamp,
    type,
    ...fields,
  };
}

function envelopeDigest(envelope) {
  return `sha256:${hashObject(envelope)}`;
}

function w009Memo(runId, proposalHash, grantDigest, authorizationDigest) {
  return [
    'covenant-safety-v2',
    `run=${runId}`,
    `proposal=${proposalHash}`,
    `grant=${grantDigest.slice(7)}`,
    `authorization=${authorizationDigest.slice(7)}`,
  ].join('|');
}

function consumptionKey(grantId, proposalHash, nonce) {
  return `sha256:${hashObject({ grant_id: grantId, proposal_hash: proposalHash, nonce })}`;
}

function safeCreatedAt(value) {
  const timestamp = Date.parse(value);
  assert(Number.isFinite(timestamp), 'created_at must be an ISO timestamp');
  assert(timestamp % 1_000 === 0, 'created_at must be rounded to a whole second');
  return new Date(timestamp).toISOString();
}

function buildMessage(publicKey, recentBlockhash, memo) {
  const feePayer = base58Decode(publicKey);
  const memoProgram = base58Decode(MEMO_PROGRAM_ID);
  const blockhash = base58Decode(recentBlockhash);
  assert(feePayer.length === 32, 'fee payer must decode to 32 bytes');
  assert(memoProgram.length === 32, 'Memo program id must decode to 32 bytes');
  assert(blockhash.length === 32, 'recent blockhash must decode to 32 bytes');
  const memoBytes = Buffer.from(memo, 'utf8');
  return Buffer.concat([
    Buffer.from([1, 0, 1]),
    encodeShortVec(2),
    feePayer,
    memoProgram,
    blockhash,
    encodeShortVec(1),
    Buffer.from([1]),
    encodeShortVec(0),
    encodeShortVec(memoBytes.length),
    memoBytes,
  ]);
}

export function createEnforcementWitness({
  runId,
  createdAt,
  expiresAt,
  feePayer,
  agentKey,
  approverKey,
  enforcerKey,
  verifierKey,
  authorityRoot,
  roleManifest,
  expectedAuthorityPublicKeyB64u,
  source,
}) {
  assert(/^[a-z0-9][a-z0-9._-]{7,127}$/.test(runId), 'run_id must be 8-128 safe characters');
  const timestamp = safeCreatedAt(createdAt);
  assert(Number.isFinite(Date.parse(expiresAt)), 'expires_at must be an ISO timestamp');
  assert(Date.parse(expiresAt) > Date.parse(timestamp), 'approval expiry must follow creation');
  assert(base58Decode(feePayer).length === 32, 'fee payer must be a Solana public key');

  const trust = verifyTrustDocuments(
    authorityRoot,
    roleManifest,
    expectedAuthorityPublicKeyB64u,
  );
  assert(trust.manifest.run_id === runId, 'trusted manifest run_id differs from witness');
  assert(trust.manifest.issued_at === timestamp, 'trusted manifest creation time differs');
  assert(trust.manifest.expires_at === expiresAt, 'trusted manifest expiry differs');
  const actors = trust.roles;
  const suppliedActors = {
    agent: actorFor('agent', agentKey),
    approver: actorFor('approver', approverKey),
    enforcer: actorFor('enforcer', enforcerKey),
    verifier: actorFor('verifier', verifierKey),
  };
  for (const role of Object.keys(suppliedActors)) {
    assert(
      canonicalJson(suppliedActors[role]) === canonicalJson(actors[role]),
      `${role} private key does not match trusted role manifest`,
    );
  }
  assert(
    base58Encode(Buffer.from(actors.agent.public_key_b64u, 'base64url')) === feePayer,
    'fee payer must be the trusted capability subject',
  );

  const nonce = sha256Hex(Buffer.from(`${runId}\napproval-nonce`, 'utf8'));
  const scope = proposalScope({ feePayer, expiresAt, nonce, runId });
  const scopeHash = `sha256:${hashObject(scope)}`;
  const proposalHash = `sha256:${hashObject({ run_id: runId, scope })}`;
  const proposal = signEvent(
    event(runId, 'w009-proposal', [], timestamp, 'transaction_proposal', {
      proposal_hash: proposalHash,
      scope,
      scope_hash: scopeHash,
    }),
    agentKey,
    actors.agent,
  );
  const deniedAttempt = signEvent(
    event(
      runId,
      'w009-sign-attempt-without-approval',
      ['w009-proposal'],
      timestamp,
      'transaction_sign_attempt',
      { proposal_hash: proposalHash, approval_grant_id: null },
    ),
    agentKey,
    actors.agent,
  );
  const denial = signEvent(
    event(
      runId,
      'w009-denial',
      ['w009-sign-attempt-without-approval'],
      timestamp,
      'authorization_decision',
      {
        proposal_hash: proposalHash,
        status: 'denied',
        reason_code: 'signed_scoped_approval_required',
      },
    ),
    enforcerKey,
    actors.enforcer,
  );
  const grant = signEvent(
    event(
      runId,
      'w009-scoped-approval',
      ['w009-proposal', 'w009-denial'],
      timestamp,
      'capability_grant',
      {
        capability: {
          action: 'solana.transaction.sign_and_submit',
          subject_key_id: actors.agent.key_id,
          subject_solana_address: feePayer,
          proposal_hash: proposalHash,
          scope,
          scope_hash: scopeHash,
          issued_at: timestamp,
          expires_at: expiresAt,
          nonce,
          max_uses: 1,
          delegation: null,
        },
      },
    ),
    approverKey,
    actors.approver,
  );
  const grantDigest = envelopeDigest(grant);
  const authorizedAttempt = signEvent(
    event(
      runId,
      'w009-sign-attempt-with-approval',
      ['w009-proposal', 'w009-scoped-approval'],
      timestamp,
      'transaction_sign_attempt',
      {
        proposal_hash: proposalHash,
        approval_grant_id: `${runId}:w009-scoped-approval`,
        approval_grant_digest: grantDigest,
        scope_hash: scopeHash,
      },
    ),
    agentKey,
    actors.agent,
  );
  const authorization = signEvent(
    event(
      runId,
      'w009-authorization',
      ['w009-sign-attempt-with-approval', 'w009-scoped-approval'],
      timestamp,
      'authorization_decision',
      {
        proposal_hash: proposalHash,
        approval_grant_id: `${runId}:w009-scoped-approval`,
        approval_grant_digest: grantDigest,
        scope_hash: scopeHash,
        status: 'authorized',
        reason_code: 'signed_scoped_approval_valid',
      },
    ),
    enforcerKey,
    actors.enforcer,
  );
  const authorizationDigest = envelopeDigest(authorization);
  const memo = w009Memo(runId, proposalHash, grantDigest, authorizationDigest);
  const instruction = instructionDescriptor(MEMO_PROGRAM_ID, [], memo);
  const transaction = {
    cluster: 'devnet',
    fee_payer: feePayer,
    instructions: [instruction.normalized],
  };
  const planHash = `sha256:${hashObject({
    proposal_hash: proposalHash,
    grant_digest: grantDigest,
    authorization_digest: authorizationDigest,
    transaction,
  })}`;
  const executionPlan = signEvent(
    event(
      runId,
      'w009-execution-plan',
      ['w009-authorization'],
      timestamp,
      'transaction_execution_plan',
      {
        proposal_hash: proposalHash,
        approval_grant_digest: grantDigest,
        authorization_digest: authorizationDigest,
        plan_hash: planHash,
        transaction,
      },
    ),
    agentKey,
    actors.agent,
  );
  const grantId = `${runId}:w009-scoped-approval`;
  const useKey = consumptionKey(grantId, proposalHash, nonce);
  const grantConsumption = signEvent(
    event(
      runId,
      'w009-grant-consumption',
      ['w009-execution-plan', 'w009-scoped-approval'],
      timestamp,
      'capability_consumption_reserved',
      {
        grant_id: grantId,
        proposal_hash: proposalHash,
        plan_hash: planHash,
        consumption_key: useKey,
        use_number: 1,
        max_uses: 1,
        status: 'reserved',
      },
    ),
    enforcerKey,
    actors.enforcer,
  );

  assert(
    sha256Hex(Buffer.from(source.data_utf8, 'utf8')) === source.data_hash,
    'source data hash does not match source data',
  );
  parseLegacyTransaction(Buffer.from(source.wire_transaction_base64 || '', 'base64'));
  const untrustedInput = signEvent(
    event(runId, 'w011-untrusted-input', [], timestamp, 'untrusted_onchain_input', {
      trust: 'untrusted',
      source: {
        cluster: 'devnet',
        transaction_signature: source.transaction_signature,
        slot: source.slot,
        block_time: source.block_time,
        confirmation_status: source.confirmation_status,
        instruction_index: source.instruction_index,
        program_id: source.program_id,
        data_utf8: source.data_utf8,
        data_hash: source.data_hash,
        wire_transaction_base64: source.wire_transaction_base64,
      },
    }),
    agentKey,
    actors.agent,
  );
  const derivedMemo = `covenant-w011-denied:${source.data_hash}`;
  const derivedInstruction = instructionDescriptor(MEMO_PROGRAM_ID, [], derivedMemo);
  assert(
    base58Decode(source.proposal_recent_blockhash).length === 32,
    'W011 proposal recent blockhash is invalid',
  );
  const recentBlockhash = source.proposal_recent_blockhash;
  const derivedTransaction = {
    cluster: 'devnet',
    fee_payer: feePayer,
    recent_blockhash: recentBlockhash,
    instructions: [derivedInstruction.normalized],
  };
  const derivedMessage = buildMessage(feePayer, recentBlockhash, derivedMemo);
  const derivedScope = {
    cluster: 'devnet',
    fee_payer: feePayer,
    recent_blockhash: recentBlockhash,
    program_hash: derivedInstruction.program_hash,
    instruction_hash: derivedInstruction.instruction_hash,
    accounts_hash: derivedInstruction.accounts_hash,
    data_hash: derivedInstruction.data_hash,
  };
  const derivedProposalHash = `sha256:${hashObject({
    run_id: runId,
    transaction: derivedTransaction,
    scope: derivedScope,
    message_base64: derivedMessage.toString('base64'),
  })}`;
  const derivedProposal = signEvent(
    event(
      runId,
      'w011-derived-proposal',
      ['w011-untrusted-input'],
      timestamp,
      'transaction_proposal_derived_from_input',
      {
        source_event_id: `${runId}:w011-untrusted-input`,
        taint: 'untrusted_onchain',
        proposed_action: 'solana.transaction.sign_and_submit',
        proposal_hash: derivedProposalHash,
        transaction: derivedTransaction,
        scope: derivedScope,
        message_base64: derivedMessage.toString('base64'),
        message_hash: `sha256:${sha256Hex(derivedMessage)}`,
      },
    ),
    agentKey,
    actors.agent,
  );
  const signAction = signEvent(
    event(
      runId,
      'w011-sign-action',
      ['w011-derived-proposal'],
      timestamp,
      'transaction_sign_action',
      {
        proposal_event_id: `${runId}:w011-derived-proposal`,
        proposal_hash: derivedProposalHash,
        status: 'requested',
      },
    ),
    agentKey,
    actors.agent,
  );
  const refutation = signEvent(
    event(
      runId,
      'w011-verifier-refutation',
      ['w011-sign-action'],
      timestamp,
      'verifier_refutation',
      {
        rule: 'W011',
        verdict: 'refute',
        reason_code: 'sign_action_descends_from_untrusted_onchain_input',
        target_event_id: `${runId}:w011-sign-action`,
        proposal_hash: derivedProposalHash,
        causal_path: [
          `${runId}:w011-untrusted-input`,
          `${runId}:w011-derived-proposal`,
          `${runId}:w011-sign-action`,
        ],
      },
    ),
    verifierKey,
    actors.verifier,
  );
  const w011Denial = signEvent(
    event(
      runId,
      'w011-enforcer-denial',
      ['w011-sign-action', 'w011-verifier-refutation'],
      timestamp,
      'authorization_decision',
      {
        rule: 'W011',
        proposal_hash: derivedProposalHash,
        status: 'denied',
        reason_code: 'untrusted_input_causal_refutation',
      },
    ),
    enforcerKey,
    actors.enforcer,
  );
  const preventedOutcome = signEvent(
    event(
      runId,
      'w011-prevented-outcome',
      ['w011-enforcer-denial'],
      timestamp,
      'transaction_execution_outcome',
      {
        proposal_hash: derivedProposalHash,
        status: 'prevented',
        signed_transaction: null,
        transaction_signature: null,
        submitted: false,
      },
    ),
    enforcerKey,
    actors.enforcer,
  );

  return {
    schema: ENFORCEMENT_SCHEMA,
    run_id: runId,
    authority: {
      root_key_id: trust.policy.authority.key_id,
      role_manifest_sha256: trust.manifestHash,
    },
    w009: {
      proposal,
      denied_sign_attempt: deniedAttempt,
      denial,
      approval_grant: grant,
      authorized_sign_attempt: authorizedAttempt,
      authorization,
      execution_plan: executionPlan,
      grant_consumption: grantConsumption,
      devnet_execution: null,
    },
    w011: {
      untrusted_input: untrustedInput,
      derived_proposal: derivedProposal,
      sign_action: signAction,
      verifier_refutation: refutation,
      enforcer_denial: w011Denial,
      prevented_outcome: preventedOutcome,
    },
  };
}

function orderedEnvelopes(bundle) {
  const values = [
    ['w009.proposal', bundle.w009?.proposal, 'agent'],
    ['w009.denied_sign_attempt', bundle.w009?.denied_sign_attempt, 'agent'],
    ['w009.denial', bundle.w009?.denial, 'enforcer'],
    ['w009.approval_grant', bundle.w009?.approval_grant, 'approver'],
    ['w009.authorized_sign_attempt', bundle.w009?.authorized_sign_attempt, 'agent'],
    ['w009.authorization', bundle.w009?.authorization, 'enforcer'],
    ['w009.execution_plan', bundle.w009?.execution_plan, 'agent'],
    ['w009.grant_consumption', bundle.w009?.grant_consumption, 'enforcer'],
    ['w011.untrusted_input', bundle.w011?.untrusted_input, 'agent'],
    ['w011.derived_proposal', bundle.w011?.derived_proposal, 'agent'],
    ['w011.sign_action', bundle.w011?.sign_action, 'agent'],
    ['w011.verifier_refutation', bundle.w011?.verifier_refutation, 'verifier'],
    ['w011.enforcer_denial', bundle.w011?.enforcer_denial, 'enforcer'],
    ['w011.prevented_outcome', bundle.w011?.prevented_outcome, 'enforcer'],
  ];
  if (bundle.w009?.devnet_execution) {
    values.push(['w009.devnet_execution', bundle.w009.devnet_execution, 'enforcer']);
  }
  return values;
}

function same(left, right) {
  return canonicalJson(left) === canonicalJson(right);
}

function expectParents(envelope, expected, label) {
  assert(same(envelope.event.parent_ids, expected), `${label} has wrong causal parents`);
}

function parseShortVec(bytes, start) {
  let value = 0;
  let shift = 0;
  let offset = start;
  while (true) {
    assert(offset < bytes.length, 'truncated shortvec');
    const byte = bytes[offset++];
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return { value, offset };
    shift += 7;
    assert(shift <= 28, 'shortvec is too large');
  }
}

function encodeShortVec(value) {
  assert(
    Number.isSafeInteger(value) && value >= 0,
    'shortvec value must be a non-negative integer',
  );
  const bytes = [];
  let remaining = value;
  do {
    let byte = remaining & 0x7f;
    remaining >>>= 7;
    if (remaining) byte |= 0x80;
    bytes.push(byte);
  } while (remaining);
  return Buffer.from(bytes);
}

function readBytes(bytes, start, length, label) {
  const end = start + length;
  assert(end <= bytes.length, `truncated ${label}`);
  return { value: bytes.subarray(start, end), offset: end };
}

function parseLegacyMessage(messageBytes) {
  const bytes = Buffer.from(messageBytes);
  const header = readBytes(bytes, 0, 3, 'message header');
  const [requiredSignatures, readonlySigned, readonlyUnsigned] = header.value;
  let cursor = parseShortVec(bytes, header.offset);
  const accountCount = cursor.value;
  const accountKeys = [];
  for (let index = 0; index < accountCount; index += 1) {
    const read = readBytes(bytes, cursor.offset, 32, 'account key');
    accountKeys.push(Buffer.from(read.value));
    cursor = { value: 0, offset: read.offset };
  }
  const blockhash = readBytes(bytes, cursor.offset, 32, 'recent blockhash');
  cursor = parseShortVec(bytes, blockhash.offset);
  const instructionCount = cursor.value;
  const instructions = [];
  for (let index = 0; index < instructionCount; index += 1) {
    const program = readBytes(bytes, cursor.offset, 1, 'program index');
    cursor = parseShortVec(bytes, program.offset);
    const accountIndexCount = cursor.value;
    const accountIndexes = readBytes(
      bytes,
      cursor.offset,
      accountIndexCount,
      'instruction account indexes',
    );
    cursor = parseShortVec(bytes, accountIndexes.offset);
    const dataLength = cursor.value;
    const data = readBytes(bytes, cursor.offset, dataLength, 'instruction data');
    cursor = { value: 0, offset: data.offset };
    assert(program.value[0] < accountCount, 'instruction program index is out of range');
    for (const accountIndex of accountIndexes.value) {
      assert(accountIndex < accountCount, 'instruction account index is out of range');
    }
    instructions.push({
      programIdIndex: program.value[0],
      accountIndexes: [...accountIndexes.value],
      data: Buffer.from(data.value),
    });
  }
  assert(cursor.offset === bytes.length, 'transaction message has trailing bytes');
  const accountMeta = accountKeys.map((publicKey, index) => {
    const isSigner = index < requiredSignatures;
    const isWritable = isSigner
      ? index < requiredSignatures - readonlySigned
      : index < accountCount - readonlyUnsigned;
    return { publicKey, isSigner, isWritable };
  });
  return {
    requiredSignatures,
    accountMeta,
    recentBlockhash: Buffer.from(blockhash.value),
    instructions,
  };
}

export function parseLegacyTransaction(wireBytes) {
  const wire = Buffer.from(wireBytes);
  let cursor = parseShortVec(wire, 0);
  const signatureCount = cursor.value;
  assert(signatureCount > 0, 'transaction has no signatures');
  const signatures = [];
  for (let index = 0; index < signatureCount; index += 1) {
    const read = readBytes(wire, cursor.offset, 64, 'transaction signature');
    signatures.push(Buffer.from(read.value));
    cursor = { value: 0, offset: read.offset };
  }
  const message = wire.subarray(cursor.offset);
  assert((message[0] & 0x80) === 0, 'only legacy Solana transactions are supported');
  const parsed = parseLegacyMessage(message);
  assert(
    parsed.requiredSignatures === signatureCount,
    'signature count does not match message header',
  );
  for (let index = 0; index < signatureCount; index += 1) {
    const publicKey = createPublicKey({
      key: Buffer.concat([ED25519_SPKI_PREFIX, parsed.accountMeta[index].publicKey]),
      format: 'der',
      type: 'spki',
    });
    assert(
      edVerify(null, message, publicKey, signatures[index]),
      `Solana signature ${index} is invalid`,
    );
  }
  return { signatures, message, ...parsed };
}

export function buildLegacyMemoTransaction(secretKeyBytes, recentBlockhash, memo) {
  const secret = Buffer.from(secretKeyBytes);
  assert(secret.length === 64, 'Solana keypair must contain 64 bytes');
  const privateKey = privateKeyFromSolanaKeypair(secret);
  const derivedPublicKey = rawPublicKey(privateKey);
  const feePayer = base58Encode(derivedPublicKey);
  const message = buildMessage(feePayer, recentBlockhash, memo);
  const signature = edSign(null, message, privateKey);
  const wire = Buffer.concat([encodeShortVec(1), signature, message]);
  parseLegacyTransaction(wire);
  return { feePayer, signature: base58Encode(signature), wire };
}

export function privateKeyFromSolanaKeypair(secretKeyBytes) {
  const secret = Buffer.from(secretKeyBytes);
  assert(secret.length === 64, 'Solana keypair must contain 64 bytes');
  const privateKey = createPrivateKey({
    key: Buffer.concat([ED25519_PKCS8_PREFIX, secret.subarray(0, 32)]),
    format: 'der',
    type: 'pkcs8',
  });
  assert(
    rawPublicKey(privateKey).equals(secret.subarray(32)),
    'Solana keypair public key does not match its seed',
  );
  return privateKey;
}

function validateMemoMessage(messageBytes, feePayer, recentBlockhash, memo, label) {
  const parsed = parseLegacyMessage(messageBytes);
  assert(parsed.requiredSignatures === 1, `${label} must require one signer`);
  assert(parsed.accountMeta.length === 2, `${label} must contain two account keys`);
  assert(
    base58Encode(parsed.accountMeta[0].publicKey) === feePayer,
    `${label} fee payer is invalid`,
  );
  assert(
    parsed.accountMeta[0].isSigner && parsed.accountMeta[0].isWritable,
    `${label} fee payer flags are invalid`,
  );
  assert(
    base58Encode(parsed.accountMeta[1].publicKey) === MEMO_PROGRAM_ID,
    `${label} program id is invalid`,
  );
  assert(
    base58Encode(parsed.recentBlockhash) === recentBlockhash,
    `${label} recent blockhash is invalid`,
  );
  assert(parsed.instructions.length === 1, `${label} must contain one instruction`);
  const instruction = parsed.instructions[0];
  assert(instruction.programIdIndex === 1, `${label} program index is invalid`);
  assert(instruction.accountIndexes.length === 0, `${label} accounts are invalid`);
  assert(instruction.data.equals(Buffer.from(memo, 'utf8')), `${label} data is invalid`);
  return parsed;
}

function validateW009(bundle, actors) {
  const run = bundle.run_id;
  const id = (suffix) => `${run}:${suffix}`;
  const proposal = bundle.w009.proposal;
  assert(proposal.event.type === 'transaction_proposal', 'W009 proposal event type is invalid');
  expectParents(proposal, [], 'W009 proposal');
  const expectedScope = proposalScope({
    feePayer: proposal.event.scope.fee_payer,
    expiresAt: proposal.event.scope.expires_at,
    nonce: proposal.event.scope.nonce,
    runId: run,
  });
  assert(same(proposal.event.scope, expectedScope), 'W009 proposal scope is invalid');
  const scopeHash = `sha256:${hashObject(expectedScope)}`;
  const proposalHash = `sha256:${hashObject({ run_id: run, scope: expectedScope })}`;
  assert(proposal.event.scope_hash === scopeHash, 'W009 proposal scope hash is invalid');
  assert(proposal.event.proposal_hash === proposalHash, 'W009 proposal hash is invalid');
  const agentAddress = base58Encode(Buffer.from(actors.agent.public_key_b64u, 'base64url'));
  assert(expectedScope.fee_payer === agentAddress, 'W009 fee payer is not the capability subject');

  const deniedAttempt = bundle.w009.denied_sign_attempt;
  assert(deniedAttempt.event.type === 'transaction_sign_attempt', 'W009 denied attempt type is invalid');
  expectParents(deniedAttempt, [id('w009-proposal')], 'W009 denied attempt');
  assert(deniedAttempt.event.proposal_hash === proposalHash, 'W009 denied attempt proposal is invalid');
  assert(deniedAttempt.event.approval_grant_id === null, 'W009 denied attempt carries approval');

  const denial = bundle.w009.denial;
  assert(denial.event.type === 'authorization_decision', 'W009 denial type is invalid');
  expectParents(denial, [id('w009-sign-attempt-without-approval')], 'W009 denial');
  assert(denial.event.proposal_hash === proposalHash, 'W009 denial proposal is invalid');
  assert(denial.event.status === 'denied', 'W009 attempt without approval was not denied');
  assert(
    denial.event.reason_code === 'signed_scoped_approval_required',
    'W009 denial reason is invalid',
  );

  const grant = bundle.w009.approval_grant;
  assert(grant.event.type === 'capability_grant', 'W009 grant type is invalid');
  expectParents(grant, [id('w009-proposal'), id('w009-denial')], 'W009 grant');
  const capability = grant.event.capability;
  assert(
    capability?.action === 'solana.transaction.sign_and_submit',
    'W009 grant action is invalid',
  );
  assert(capability.subject_key_id === actors.agent.key_id, 'W009 subject key id is invalid');
  assert(capability.subject_solana_address === agentAddress, 'W009 subject address is invalid');
  assert(capability.proposal_hash === proposalHash, 'W009 grant proposal is invalid');
  assert(same(capability.scope, expectedScope), 'W009 grant scope differs from proposal');
  assert(capability.scope_hash === scopeHash, 'W009 grant scope hash is invalid');
  assert(capability.issued_at === proposal.event.timestamp, 'W009 grant issuance is invalid');
  assert(capability.expires_at === expectedScope.expires_at, 'W009 grant expiry is invalid');
  assert(capability.nonce === expectedScope.nonce, 'W009 grant nonce is invalid');
  assert(capability.max_uses === 1 && capability.scope.max_uses === 1, 'W009 grant is not one-use');
  assert(capability.delegation === null, 'W009 reference grant must not delegate signing');
  const grantDigest = envelopeDigest(grant);

  const authorizedAttempt = bundle.w009.authorized_sign_attempt;
  assert(
    authorizedAttempt.event.type === 'transaction_sign_attempt',
    'W009 authorized attempt type is invalid',
  );
  expectParents(
    authorizedAttempt,
    [id('w009-proposal'), id('w009-scoped-approval')],
    'W009 authorized attempt',
  );
  assert(authorizedAttempt.event.proposal_hash === proposalHash, 'W009 authorized proposal is invalid');
  assert(
    authorizedAttempt.event.approval_grant_id === id('w009-scoped-approval'),
    'W009 authorized attempt grant id is invalid',
  );
  assert(
    authorizedAttempt.event.approval_grant_digest === grantDigest,
    'W009 authorized attempt grant digest is invalid',
  );
  assert(authorizedAttempt.event.scope_hash === scopeHash, 'W009 authorized scope hash is invalid');

  const authorization = bundle.w009.authorization;
  assert(authorization.event.type === 'authorization_decision', 'W009 authorization type is invalid');
  expectParents(
    authorization,
    [id('w009-sign-attempt-with-approval'), id('w009-scoped-approval')],
    'W009 authorization',
  );
  assert(authorization.event.proposal_hash === proposalHash, 'W009 authorization proposal is invalid');
  assert(
    authorization.event.approval_grant_id === id('w009-scoped-approval'),
    'W009 authorization grant id is invalid',
  );
  assert(
    authorization.event.approval_grant_digest === grantDigest,
    'W009 authorization grant digest is invalid',
  );
  assert(authorization.event.scope_hash === scopeHash, 'W009 authorization scope hash is invalid');
  assert(authorization.event.status === 'authorized', 'W009 approved attempt was not authorized');
  assert(
    authorization.event.reason_code === 'signed_scoped_approval_valid',
    'W009 authorization reason is invalid',
  );
  const authorizationTime = Date.parse(authorization.event.timestamp);
  assert(
    authorizationTime >= Date.parse(capability.issued_at) &&
      authorizationTime <= Date.parse(capability.expires_at),
    'W009 authorization falls outside grant validity',
  );
  const authorizationDigest = envelopeDigest(authorization);

  const executionPlan = bundle.w009.execution_plan;
  assert(
    executionPlan.event.type === 'transaction_execution_plan',
    'W009 execution plan type is invalid',
  );
  expectParents(executionPlan, [id('w009-authorization')], 'W009 execution plan');
  assert(executionPlan.event.proposal_hash === proposalHash, 'W009 execution plan proposal is invalid');
  assert(
    executionPlan.event.approval_grant_digest === grantDigest,
    'W009 execution plan grant digest is invalid',
  );
  assert(
    executionPlan.event.authorization_digest === authorizationDigest,
    'W009 execution plan authorization digest is invalid',
  );
  const memo = w009Memo(run, proposalHash, grantDigest, authorizationDigest);
  const expectedInstruction = instructionDescriptor(MEMO_PROGRAM_ID, [], memo);
  const expectedTransaction = {
    cluster: 'devnet',
    fee_payer: agentAddress,
    instructions: [expectedInstruction.normalized],
  };
  assert(same(executionPlan.event.transaction, expectedTransaction), 'W009 memo commitments are invalid');
  const planHash = `sha256:${hashObject({
    proposal_hash: proposalHash,
    grant_digest: grantDigest,
    authorization_digest: authorizationDigest,
    transaction: expectedTransaction,
  })}`;
  assert(executionPlan.event.plan_hash === planHash, 'W009 execution plan hash is invalid');

  const grantConsumption = bundle.w009.grant_consumption;
  assert(
    grantConsumption.event.type === 'capability_consumption_reserved',
    'W009 grant consumption type is invalid',
  );
  expectParents(
    grantConsumption,
    [id('w009-execution-plan'), id('w009-scoped-approval')],
    'W009 grant consumption',
  );
  const expectedUseKey = consumptionKey(id('w009-scoped-approval'), proposalHash, expectedScope.nonce);
  assert(grantConsumption.event.grant_id === id('w009-scoped-approval'), 'W009 consumed grant is invalid');
  assert(grantConsumption.event.proposal_hash === proposalHash, 'W009 consumption proposal is invalid');
  assert(grantConsumption.event.plan_hash === planHash, 'W009 consumption plan is invalid');
  assert(
    grantConsumption.event.consumption_key === expectedUseKey,
    'W009 consumption key is invalid',
  );
  assert(
    grantConsumption.event.use_number === 1 &&
      grantConsumption.event.max_uses === 1 &&
      grantConsumption.event.status === 'reserved',
    'W009 consumption is not an exact one-use reservation',
  );
  return {
    proposalHash,
    grantDigest,
    authorizationDigest,
    planHash,
    memo,
    agentAddress,
    authorizationTime,
    expiresAt: Date.parse(capability.expires_at),
    consumptionKey: expectedUseKey,
  };
}

function validateSource(source) {
  assert(source?.cluster === 'devnet', 'W011 source cluster is invalid');
  assert(base58Decode(source.transaction_signature).length === 64, 'W011 source signature is invalid');
  assert(Number.isSafeInteger(source.slot) && source.slot > 0, 'W011 source slot is invalid');
  assert(Number.isSafeInteger(source.block_time) && source.block_time > 0, 'W011 source block time is invalid');
  assert(source.confirmation_status === 'finalized', 'W011 source must be finalized');
  assert(
    Number.isSafeInteger(source.instruction_index) && source.instruction_index >= 0,
    'W011 source instruction index is invalid',
  );
  assert(source.program_id === MEMO_PROGRAM_ID, 'W011 source program is invalid');
  assert(
    sha256Hex(Buffer.from(source.data_utf8, 'utf8')) === source.data_hash,
    'W011 source data hash is invalid',
  );
  const parsed = parseLegacyTransaction(Buffer.from(source.wire_transaction_base64 || '', 'base64'));
  assert(
    base58Encode(parsed.signatures[0]) === source.transaction_signature,
    'W011 source signature differs from wire bytes',
  );
  assert(source.instruction_index < parsed.instructions.length, 'W011 source instruction is out of range');
  const instruction = parsed.instructions[source.instruction_index];
  assert(
    base58Encode(parsed.accountMeta[instruction.programIdIndex].publicKey) === source.program_id,
    'W011 source program differs from wire bytes',
  );
  assert(
    instruction.data.equals(Buffer.from(source.data_utf8, 'utf8')),
    'W011 source data differs from wire bytes',
  );
  return parsed;
}

function validateW011(bundle, agentAddress) {
  const run = bundle.run_id;
  const id = (suffix) => `${run}:${suffix}`;
  const untrusted = bundle.w011.untrusted_input;
  assert(untrusted.event.type === 'untrusted_onchain_input', 'W011 input type is invalid');
  expectParents(untrusted, [], 'W011 input');
  assert(untrusted.event.trust === 'untrusted', 'W011 input is not marked untrusted');
  const sourceTransaction = validateSource(untrusted.event.source);

  const derived = bundle.w011.derived_proposal;
  assert(
    derived.event.type === 'transaction_proposal_derived_from_input',
    'W011 proposal type is invalid',
  );
  expectParents(derived, [id('w011-untrusted-input')], 'W011 proposal');
  assert(derived.event.source_event_id === id('w011-untrusted-input'), 'W011 source event is invalid');
  assert(derived.event.taint === 'untrusted_onchain', 'W011 proposal lost its taint');
  assert(
    derived.event.proposed_action === 'solana.transaction.sign_and_submit',
    'W011 proposal action is invalid',
  );
  const memo = `covenant-w011-denied:${untrusted.event.source.data_hash}`;
  const instruction = instructionDescriptor(MEMO_PROGRAM_ID, [], memo);
  const recentBlockhash = derived.event.transaction?.recent_blockhash;
  assert(
    base58Decode(recentBlockhash).length === 32,
    'W011 proposed recent blockhash is invalid',
  );
  const transaction = {
    cluster: 'devnet',
    fee_payer: agentAddress,
    recent_blockhash: recentBlockhash,
    instructions: [instruction.normalized],
  };
  const scope = {
    cluster: 'devnet',
    fee_payer: agentAddress,
    recent_blockhash: recentBlockhash,
    program_hash: instruction.program_hash,
    instruction_hash: instruction.instruction_hash,
    accounts_hash: instruction.accounts_hash,
    data_hash: instruction.data_hash,
  };
  assert(same(derived.event.transaction, transaction), 'W011 transaction descriptor is invalid');
  assert(same(derived.event.scope, scope), 'W011 transaction scope is invalid');
  const message = Buffer.from(derived.event.message_base64 || '', 'base64');
  validateMemoMessage(message, agentAddress, recentBlockhash, memo, 'W011 proposed message');
  assert(
    derived.event.message_hash === `sha256:${sha256Hex(message)}`,
    'W011 message hash is invalid',
  );
  const proposalHash = `sha256:${hashObject({
    run_id: run,
    transaction,
    scope,
    message_base64: message.toString('base64'),
  })}`;
  assert(derived.event.proposal_hash === proposalHash, 'W011 proposal hash is invalid');

  const signAction = bundle.w011.sign_action;
  assert(signAction.event.type === 'transaction_sign_action', 'W011 sign action type is invalid');
  expectParents(signAction, [id('w011-derived-proposal')], 'W011 sign action');
  assert(signAction.event.proposal_event_id === id('w011-derived-proposal'), 'W011 sign target is invalid');
  assert(signAction.event.proposal_hash === proposalHash, 'W011 sign proposal hash is invalid');
  assert(signAction.event.status === 'requested', 'W011 sign action was not requested');

  const refutation = bundle.w011.verifier_refutation;
  assert(refutation.event.type === 'verifier_refutation', 'W011 refutation type is invalid');
  expectParents(refutation, [id('w011-sign-action')], 'W011 refutation');
  assert(refutation.event.rule === 'W011', 'W011 refutation rule is invalid');
  assert(refutation.event.verdict === 'refute', 'W011 verifier did not refute');
  assert(
    refutation.event.reason_code === 'sign_action_descends_from_untrusted_onchain_input',
    'W011 refutation reason is invalid',
  );
  assert(refutation.event.target_event_id === id('w011-sign-action'), 'W011 refutation target is invalid');
  assert(refutation.event.proposal_hash === proposalHash, 'W011 refutation proposal is invalid');
  assert(
    same(refutation.event.causal_path, [
      id('w011-untrusted-input'),
      id('w011-derived-proposal'),
      id('w011-sign-action'),
    ]),
    'W011 refutation causal path is invalid',
  );

  const denial = bundle.w011.enforcer_denial;
  assert(denial.event.type === 'authorization_decision', 'W011 denial type is invalid');
  expectParents(
    denial,
    [id('w011-sign-action'), id('w011-verifier-refutation')],
    'W011 denial',
  );
  assert(denial.event.rule === 'W011', 'W011 denial rule is invalid');
  assert(denial.event.proposal_hash === proposalHash, 'W011 denial proposal is invalid');
  assert(denial.event.status === 'denied', 'W011 enforcer did not deny signing');
  assert(
    denial.event.reason_code === 'untrusted_input_causal_refutation',
    'W011 denial reason is invalid',
  );

  const outcome = bundle.w011.prevented_outcome;
  assert(outcome.event.type === 'transaction_execution_outcome', 'W011 outcome type is invalid');
  expectParents(outcome, [id('w011-enforcer-denial')], 'W011 outcome');
  assert(outcome.event.proposal_hash === proposalHash, 'W011 outcome proposal is invalid');
  assert(outcome.event.status === 'prevented', 'W011 outcome was not prevented');
  assert(outcome.event.signed_transaction === null, 'W011 outcome contains signed bytes');
  assert(outcome.event.transaction_signature === null, 'W011 outcome contains a signature');
  assert(outcome.event.submitted === false, 'W011 outcome claims submission');
  return {
    source: untrusted.event.source,
    proposalHash,
    sourceSignatureCount: sourceTransaction.signatures.length,
  };
}

function reservationBytes(evidence) {
  const record = evidence?.record;
  if (evidence?.scheme === 'canonical_exclusive_fsync_file.v1') {
    return Buffer.from(`${canonicalJson(record)}\n`, 'utf8');
  }
  if (evidence?.scheme === 'legacy_exclusive_file.v0') {
    return Buffer.from(
      `${JSON.stringify({
        run_id: record?.run_id,
        consumption_key: record?.consumption_key,
        reserved_at: record?.reserved_at,
      })}\n`,
      'utf8',
    );
  }
  throw new Error('W009 durable reservation scheme is invalid');
}

function validateReservationEvidence(bundle, evidence, w009, blockTime) {
  const record = evidence?.record;
  assert(record?.run_id === bundle.run_id, 'W009 durable reservation run_id is invalid');
  assert(
    record.consumption_key === w009.consumptionKey,
    'W009 durable reservation consumption key is invalid',
  );
  if (evidence.scheme === 'canonical_exclusive_fsync_file.v1') {
    assert(
      record.schema === 'covenant.grant-consumption-reservation.v1',
      'W009 durable reservation record schema is invalid',
    );
    assert(
      record.proposal_hash === w009.proposalHash,
      'W009 durable reservation proposal hash is invalid',
    );
  }
  const reservedAt = Date.parse(record.reserved_at);
  assert(Number.isFinite(reservedAt), 'W009 durable reservation timestamp is invalid');
  assert(reservedAt <= w009.expiresAt, 'W009 durable reservation occurred after grant expiry');
  assert(
    reservedAt <= blockTime * 1_000 + 5_000,
    'W009 durable reservation timestamp is implausibly after execution',
  );
  assert(
    evidence.record_sha256 === `sha256:${sha256Hex(reservationBytes(evidence))}`,
    'W009 durable reservation digest is invalid',
  );
  return evidence.scheme;
}

function validateDevnetExecution(bundle, w009) {
  const envelope = bundle.w009.devnet_execution;
  if (!envelope) return null;
  const record = envelope.event;
  assert(record.type === 'transaction_execution_record', 'W009 execution record type is invalid');
  expectParents(
    envelope,
    [`${bundle.run_id}:w009-grant-consumption`],
    'W009 execution record',
  );
  assert(record.proposal_hash === w009.proposalHash, 'W009 execution proposal is invalid');
  assert(record.plan_hash === w009.planHash, 'W009 execution plan hash is invalid');
  assert(record.consumption_key === w009.consumptionKey, 'W009 execution consumption is invalid');
  assert(Number.isSafeInteger(record.slot) && record.slot > 0, 'W009 execution slot is invalid');
  assert(
    Number.isSafeInteger(record.block_time) && record.block_time > 0,
    'W009 execution block time is invalid',
  );
  assert(record.confirmation_status === 'finalized', 'W009 execution is not finalized');
  assert(record.execution_status === 'succeeded', 'W009 execution did not succeed');
  const parsed = parseLegacyTransaction(Buffer.from(record.wire_transaction_base64 || '', 'base64'));
  assert(parsed.signatures.length === 1, 'W009 transaction must have one signer');
  assert(
    base58Encode(parsed.signatures[0]) === record.transaction_signature,
    'W009 transaction signature differs from wire bytes',
  );
  assert(
    base58Encode(parsed.accountMeta[0].publicKey) === w009.agentAddress,
    'W009 transaction signer is not the capability subject',
  );
  validateMemoMessage(
    parsed.message,
    w009.agentAddress,
    base58Encode(parsed.recentBlockhash),
    w009.memo,
    'W009 transaction',
  );
  const blockTimeMs = record.block_time * 1_000;
  assert(
    w009.authorizationTime <= blockTimeMs,
    'W009 transaction landed before authorization',
  );
  assert(blockTimeMs <= w009.expiresAt, 'W009 transaction landed after grant expiry');
  assert(
    Date.parse(record.timestamp) >= blockTimeMs,
    'W009 execution record predates the transaction block time',
  );
  const reservationScheme = validateReservationEvidence(
    bundle,
    record.durable_reservation,
    w009,
    record.block_time,
  );
  return { reservationScheme };
}

export function verifyEnforcementWitness(
  bundle,
  {
    authorityRoot,
    roleManifest,
    expectedAuthorityPublicKeyB64u,
    requireDevnetRecord = false,
  } = {},
) {
  assert(bundle?.schema === ENFORCEMENT_SCHEMA, 'unsupported enforcement witness schema');
  assert(/^[a-z0-9][a-z0-9._-]{7,127}$/.test(bundle.run_id), 'bundle run_id is invalid');
  const trust = verifyTrustDocuments(
    authorityRoot,
    roleManifest,
    expectedAuthorityPublicKeyB64u,
  );
  assert(trust.manifest.run_id === bundle.run_id, 'bundle run_id differs from trusted manifest');
  assert(
    bundle.authority?.root_key_id === trust.policy.authority.key_id,
    'bundle authority root reference is invalid',
  );
  assert(
    bundle.authority?.role_manifest_sha256 === trust.manifestHash,
    'bundle role manifest reference is invalid',
  );

  const seen = new Set();
  let previousTimestamp = 0;
  const envelopes = orderedEnvelopes(bundle);
  for (const [label, envelope, role] of envelopes) {
    verifyEnvelope(envelope, trust.roles[role], label);
    const current = envelope.event;
    assert(current.run_id === bundle.run_id, `${label} has wrong run_id`);
    assert(!seen.has(current.id), `${label} reuses an event id`);
    assert(Array.isArray(current.parent_ids), `${label} parent_ids must be an array`);
    for (const parentId of current.parent_ids) {
      assert(seen.has(parentId), `${label} references a missing or forward causal parent`);
    }
    const timestamp = Date.parse(current.timestamp);
    assert(
      Number.isFinite(timestamp) && timestamp >= previousTimestamp,
      `${label} timestamp breaks event order`,
    );
    assert(
      timestamp >= Date.parse(trust.manifest.issued_at) &&
        timestamp <= Date.parse(trust.manifest.expires_at),
      `${label} falls outside trusted manifest validity`,
    );
    previousTimestamp = timestamp;
    seen.add(current.id);
  }

  const w009 = validateW009(bundle, trust.roles);
  const w011 = validateW011(bundle, w009.agentAddress);
  const devnetExecution = validateDevnetExecution(bundle, w009);
  assert(!requireDevnetRecord || devnetExecution, 'a finalized devnet execution record is required');
  return {
    schema: bundle.schema,
    run_id: bundle.run_id,
    evidence_mode: trust.manifest.evidence_mode,
    trust: {
      authority_root: 'pinned',
      role_manifest: 'root_signed_and_hash_pinned',
    },
    signatures_verified: {
      trust_documents: 2,
      signed_events: envelopes.length,
      solana_wire: w011.sourceSignatureCount + (devnetExecution ? 1 : 0),
    },
    w009: {
      unauthorized_attempt: 'denied',
      scoped_approval: 'verified',
      capability_subject_is_solana_signer: true,
      signed_one_use_reservation_claim: 'verified',
      runtime_replay_guard: 'not_observable_from_static_bundle',
      durable_reservation_evidence: devnetExecution
        ? devnetExecution.reservationScheme
        : 'not_recorded',
      offline_wire_execution: devnetExecution ? 'verified' : 'not_recorded',
      live_rpc_confirmation: 'not_checked',
    },
    w011: {
      source: 'untrusted_devnet_input',
      concrete_transaction_bytes: 'verified',
      causal_lineage: 'verified',
      separately_keyed_refutation: 'verified',
      enforcer_denial: 'verified',
      signed_no_submit_outcome: 'verified',
      callback_behavior: 'not_observable_from_static_bundle',
      live_rpc_confirmation: 'not_checked',
    },
    boundary:
      'This proves the standalone reference harness and signed artifact chain, not mediation by a production daemon or external wallet.',
  };
}

export async function executeAuthorizedW009(options) {
  assert(options && typeof options === 'object', 'W009 execution options are required');
  for (const forbidden of ['grantUseStore', 'stateDirectory', 'journal', 'consumptionDirectory']) {
    assert(
      !Object.hasOwn(options, forbidden),
      'W009 execution state namespace is module-owned',
    );
  }
  const { bundle, trust, secretKey, recentBlockhash, submit } = options;
  const summary = verifyEnforcementWitness(bundle, trust);
  assert(bundle.w009.devnet_execution === null, 'W009 bundle already contains an execution record');
  assert(typeof submit === 'function', 'W009 submit callback is required');
  const expiresAt = Date.parse(bundle.w009.approval_grant.event.capability.expires_at);
  const assertUnexpired = () => {
    assert(Date.now() <= expiresAt, 'W009 grant expired before execution');
  };
  assertUnexpired();
  const useKey = bundle.w009.grant_consumption.event.consumption_key;
  const reservationEvidence = await reserveCanonicalConsumption({
    runId: bundle.run_id,
    consumptionKey: useKey,
    proposalHash: bundle.w009.proposal.event.proposal_hash,
  });
  assertUnexpired();
  const memo = bundle.w009.execution_plan.event.transaction.instructions[0].data;
  const transaction = buildLegacyMemoTransaction(secretKey, recentBlockhash, memo);
  assert(
    transaction.feePayer === bundle.w009.proposal.event.scope.fee_payer,
    'W009 signing key is not the capability subject',
  );
  assertUnexpired();
  const result = await submit(transaction);
  return { summary, transaction, reservationEvidence, result };
}

export async function enforceW011({ bundle, trust, submit }) {
  const summary = verifyEnforcementWitness(bundle, trust);
  assert(typeof submit === 'function', 'W011 submit callback is required');
  assert(
    bundle.w011.prevented_outcome.event.submitted === false,
    'W011 prevented outcome is invalid',
  );
  return {
    summary,
    status: 'prevented',
    submit_callback_called: false,
  };
}

export function createDevnetExecutionEnvelope({
  bundle,
  trust,
  enforcerKey,
  transaction,
  slot,
  blockTime,
  recordedAt,
  reservationEvidence,
}) {
  const summary = verifyEnforcementWitness(bundle, trust);
  assert(summary.w009.offline_wire_execution === 'not_recorded', 'W009 execution already recorded');
  const enforcer = trust.roleManifest.payload.roles.enforcer;
  assert(
    same(actorFor('enforcer', enforcerKey), enforcer),
    'execution recorder key does not match trusted enforcer',
  );
  const parsed = parseLegacyTransaction(transaction.wire);
  assert(
    base58Encode(parsed.signatures[0]) === transaction.signature,
    'execution transaction signature is invalid',
  );
  const eventTimestamp = safeCreatedAt(recordedAt);
  assert(
    reservationEvidence?.scheme === 'canonical_exclusive_fsync_file.v1' ||
      reservationEvidence?.scheme === 'legacy_exclusive_file.v0',
    'execution record requires durable reservation evidence',
  );
  validateReservationEvidence(
    bundle,
    reservationEvidence,
    {
      proposalHash: bundle.w009.proposal.event.proposal_hash,
      consumptionKey: bundle.w009.grant_consumption.event.consumption_key,
      expiresAt: Date.parse(bundle.w009.approval_grant.event.capability.expires_at),
    },
    blockTime,
  );
  return signEvent(
    event(
      bundle.run_id,
      'w009-devnet-execution',
      ['w009-grant-consumption'],
      eventTimestamp,
      'transaction_execution_record',
      {
        proposal_hash: bundle.w009.proposal.event.proposal_hash,
        plan_hash: bundle.w009.execution_plan.event.plan_hash,
        consumption_key: bundle.w009.grant_consumption.event.consumption_key,
        transaction_signature: transaction.signature,
        slot,
        block_time: blockTime,
        confirmation_status: 'finalized',
        execution_status: 'succeeded',
        durable_reservation: reservationEvidence,
        wire_transaction_base64: transaction.wire.toString('base64'),
      },
    ),
    enforcerKey,
    enforcer,
  );
}

export async function verifyRpcEvidence(bundle, rpc, trust) {
  const offline = verifyEnforcementWitness(bundle, {
    ...trust,
    requireDevnetRecord: true,
  });
  const genesisHash = await rpc('getGenesisHash');
  assert(genesisHash === DEVNET_GENESIS_HASH, 'RPC is not Solana devnet');

  async function verifyRecord(record, label) {
    const transaction = await rpc('getTransaction', [
      record.transaction_signature,
      { encoding: 'base64', commitment: 'finalized', maxSupportedTransactionVersion: 0 },
    ]);
    assert(transaction, `${label} transaction is missing from RPC`);
    assert(transaction.meta?.err === null, `${label} transaction failed onchain`);
    assert(transaction.slot === record.slot, `${label} slot does not match RPC`);
    assert(transaction.blockTime === record.block_time, `${label} block time does not match RPC`);
    assert(
      transaction.transaction?.[0] === record.wire_transaction_base64,
      `${label} wire bytes do not match RPC`,
    );
    const statuses = await rpc('getSignatureStatuses', [
      [record.transaction_signature],
      { searchTransactionHistory: true },
    ]);
    const status = statuses?.value?.[0];
    assert(status?.err === null, `${label} signature status reports failure`);
    assert(
      status?.confirmationStatus === record.confirmation_status,
      `${label} confirmation status does not match RPC`,
    );
  }

  await verifyRecord(bundle.w011.untrusted_input.event.source, 'W011 source');
  await verifyRecord(bundle.w009.devnet_execution.event, 'W009 execution');
  return {
    ...offline,
    w009: { ...offline.w009, live_rpc_confirmation: 'verified' },
    w011: { ...offline.w011, live_rpc_confirmation: 'verified' },
    rpc: { cluster: 'devnet', genesis_hash: genesisHash, exact_records: 'verified' },
  };
}
