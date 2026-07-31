import { test } from 'node:test';
import assert from 'node:assert/strict';
import bs58 from 'bs58';
import { Attestor, verifyAttestation } from './attest.js';

const seed = Array.from({ length: 32 }, (_, index) => index + 1);

test('verifies only against the externally pinned publisher key', () => {
  const attestor = new Attestor(seed);
  const statement = attestor.attest('subject', { delivered: true }, 123);

  assert.equal(verifyAttestation(statement, attestor.pubkeyB58), true);
  assert.equal(verifyAttestation(statement, bs58.encode(Buffer.alloc(32, 9))), false);
});

test('rejects a valid attacker self-signature', () => {
  const trusted = new Attestor(seed);
  const attacker = new Attestor(Array.from({ length: 32 }, (_, index) => index + 2));
  const statement = attacker.attest('subject', { delivered: true }, 123);

  assert.equal(verifyAttestation(statement, trusted.pubkeyB58), false);
});

test('rejects claim and protocol mutations', () => {
  const attestor = new Attestor(seed);
  const statement = attestor.attest('subject', { delivered: true }, 123);

  assert.equal(
    verifyAttestation(
      { ...statement, payload: { ...statement.payload, claim: { delivered: false } } },
      attestor.pubkeyB58,
    ),
    false,
  );
  assert.equal(
    verifyAttestation({ ...statement, domain: 'attacker.v1' }, attestor.pubkeyB58),
    false,
  );
  assert.equal(verifyAttestation({ ...statement, alg: 'other' }, attestor.pubkeyB58), false);
});

test('validates seed-plus-public-key material', () => {
  const attestor = new Attestor(seed);
  const publicKey = [...bs58.decode(attestor.pubkeyB58)];

  assert.equal(new Attestor([...seed, ...publicKey]).pubkeyB58, attestor.pubkeyB58);
  assert.throws(() => new Attestor([...seed, ...Buffer.alloc(32, 9)]), /public half/);
  assert.throws(() => new Attestor([256, ...seed.slice(1)]), /32-byte seed/);
  assert.throws(() => new Attestor(seed.slice(1)), /32-byte seed/);
});
