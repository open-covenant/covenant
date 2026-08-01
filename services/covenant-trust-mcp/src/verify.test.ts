import assert from 'node:assert/strict';
import {createHash, generateKeyPairSync, sign} from 'node:crypto';
import {describe, it} from 'node:test';
import bs58 from 'bs58';
import {canonical, verifyAttestation, type Attestation} from './verify.js';

const CANONICALIZATION = 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8';

function signedAttestation(): Attestation {
  const {publicKey, privateKey} = generateKeyPairSync('ed25519');
  const payload = {subject: 'did:example:agent', claim: {role: 'worker'}, ts: 1_750_000_000};
  const digest = createHash('sha256').update(canonical(payload), 'utf8').digest('hex');
  const signature = sign(null, Buffer.from(`covenant.attest.v1\n${digest}`, 'utf8'), privateKey);
  const jwk = publicKey.export({format: 'jwk'});

  return {
    alg: 'ed25519',
    domain: 'covenant.attest.v1',
    canonicalization: CANONICALIZATION,
    payload,
    digest_sha256_hex: digest,
    pubkey_b58: bs58.encode(Buffer.from(jwk.x!, 'base64url')),
    signature_b58: bs58.encode(signature),
  };
}

describe('verifyAttestation', () => {
  it('verifies integrity without inventing a trust decision', () => {
    const attestation = signedAttestation();
    assert.deepEqual(verifyAttestation(attestation), {
      ok: true,
      subject: 'did:example:agent',
      signer: attestation.pubkey_b58,
      signatureValid: true,
      signerMatches: null,
    });
  });

  it('reports whether an independently supplied signer matches', () => {
    const attestation = signedAttestation();
    assert.equal(verifyAttestation(attestation, attestation.pubkey_b58).ok, true);
    const result = verifyAttestation(attestation, bs58.encode(Buffer.alloc(32, 7)));
    assert.equal(result.ok, true);
    if (result.ok) assert.equal(result.signerMatches, false);
    const empty = verifyAttestation(attestation, '');
    assert.equal(empty.ok, true);
    if (empty.ok) assert.equal(empty.signerMatches, false);
  });

  it('rejects a modified payload', () => {
    const attestation = signedAttestation();
    attestation.payload = {...attestation.payload, subject: 'did:example:attacker'};
    assert.deepEqual(verifyAttestation(attestation), {
      ok: false,
      reason: 'digest does not match payload (tampered)',
    });
  });

  it('rejects envelopes from another signing domain', () => {
    const attestation = signedAttestation();
    attestation.domain = 'example.attest.v1';
    assert.deepEqual(verifyAttestation(attestation), {
      ok: false,
      reason: 'unsupported domain: example.attest.v1',
    });
  });

  it('rejects signed payloads that do not satisfy the attestation contract', () => {
    const attestation = signedAttestation();
    const malformed = {
      ...attestation,
      payload: {claim: {role: 'worker'}, ts: -1},
    } as unknown as Attestation;
    malformed.digest_sha256_hex = createHash('sha256')
      .update(canonical(malformed.payload), 'utf8')
      .digest('hex');

    assert.deepEqual(verifyAttestation(malformed), {
      ok: false,
      reason: 'payload requires a non-empty subject, claim, and non-negative integer ts',
    });
  });

  it('rejects non-JSON values instead of silently omitting them', () => {
    assert.throws(() => canonical({subject: 'agent', claim: undefined, ts: 1}), /undefined/);
  });
});
