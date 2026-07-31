import assert from 'node:assert/strict';
import {createHash, generateKeyPairSync, sign} from 'node:crypto';
import {after, before, describe, it} from 'node:test';
import type {AddressInfo} from 'node:net';
import type {Server} from 'node:http';
import bs58 from 'bs58';
import {createApp} from './server.js';
import {canonical} from './verify.js';

let server: Server;
let origin: string;

function signedAttestation() {
  const {publicKey, privateKey} = generateKeyPairSync('ed25519');
  const payload = {subject: 'did:example:agent', claim: {role: 'worker'}, ts: 1_750_000_000};
  const digest = createHash('sha256').update(canonical(payload), 'utf8').digest('hex');
  const signature = sign(null, Buffer.from(`covenant.attest.v1\n${digest}`, 'utf8'), privateKey);
  const jwk = publicKey.export({format: 'jwk'});
  return {
    alg: 'ed25519',
    domain: 'covenant.attest.v1',
    canonicalization: 'JSON, recursively key-sorted, no insignificant whitespace, UTF-8',
    payload,
    digest_sha256_hex: digest,
    pubkey_b58: bs58.encode(Buffer.from(jwk.x!, 'base64url')),
    signature_b58: bs58.encode(signature),
  };
}

before(async () => {
  server = await new Promise((resolve) => {
    const current = createApp().listen(0, '127.0.0.1', () => resolve(current));
  });
  const address = server.address() as AddressInfo;
  origin = `http://127.0.0.1:${address.port}`;
});

after(async () => {
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
});

describe('HTTP service', () => {
  it('publishes health, service policy, and OpenAPI metadata', async () => {
    const health = await fetch(`${origin}/health`);
    assert.equal(health.status, 200);
    assert.deepEqual(await health.json(), {
      ok: true,
      service: 'covenant-trust',
      version: '0.2.0',
    });

    const root = await fetch(origin);
    const descriptor = (await root.json()) as {policy: string};
    assert.match(descriptor.policy, /caller decides/);

    const openapi = await fetch(`${origin}/openapi.json`);
    const document = (await openapi.json()) as {openapi: string; paths: Record<string, unknown>};
    assert.equal(document.openapi, '3.1.0');
    assert.ok(document.paths['/v1/agents/{asset}']);
    assert.ok(document.paths['/v1/attestations/verify']);
  });

  it('verifies signatures without treating an arbitrary key as trusted', async () => {
    const attestation = signedAttestation();
    const response = await fetch(`${origin}/v1/attestations/verify`, {
      method: 'POST',
      headers: {'content-type': 'application/json'},
      body: JSON.stringify({attestation}),
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), {
      ok: true,
      subject: 'did:example:agent',
      signer: attestation.pubkey_b58,
      signatureValid: true,
      signerMatches: null,
    });
  });

  it('rejects malformed addresses before making RPC calls', async () => {
    const history = await fetch(`${origin}/v1/payment-history/not-an-address`);
    assert.equal(history.status, 400);

    const passport = await fetch(`${origin}/v1/agents/not-an-address`);
    assert.equal(passport.status, 400);
  });
});
