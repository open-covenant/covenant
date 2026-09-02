import assert from 'node:assert/strict';
import {
  createPublicKey,
  generateKeyPairSync,
  randomBytes,
  verify as edVerify,
} from 'node:crypto';
import { test } from 'node:test';
import { cdpAuthHeaders, cdpFeePayer, cdpPrivateKey, mintCdpJwt } from './cdp.js';

/** A CDP-shaped secret: 32-byte seed followed by its public key, base64. */
function testSecret() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const seed = privateKey.export({ format: 'jwk' }).d as string;
  const pub = publicKey.export({ format: 'jwk' }).x as string;
  return Buffer.concat([
    Buffer.from(seed, 'base64url'),
    Buffer.from(pub, 'base64url'),
  ]).toString('base64');
}

const decode = (segment: string) => JSON.parse(Buffer.from(segment, 'base64url').toString('utf8'));

test('rejects a secret that is not the 64 bytes CDP issues', () => {
  assert.throws(
    () => cdpPrivateKey(randomBytes(32).toString('base64')),
    /decodes to 32 bytes, expected 64/,
  );
});

test('binds each token to one method and path, which is why headers are keyed', async () => {
  const secret = testSecret();
  const headers = await cdpAuthHeaders('key-id', secret)();

  const uriFor = (bucket: Record<string, string>) =>
    decode(bucket.Authorization.replace('Bearer ', '').split('.')[1]).uris;

  assert.deepEqual(uriFor(headers.verify), ['POST api.cdp.coinbase.com/platform/v2/x402/verify']);
  assert.deepEqual(uriFor(headers.settle), ['POST api.cdp.coinbase.com/platform/v2/x402/settle']);
  assert.deepEqual(uriFor(headers.supported), [
    'GET api.cdp.coinbase.com/platform/v2/x402/supported',
  ]);
  // A token minted for one path is refused at another, so a single shared
  // header would authenticate exactly one of these calls.
  assert.notEqual(headers.verify.Authorization, headers.settle.Authorization);
});

test('signs the token with the issued key', () => {
  const secret = testSecret();
  const raw = Buffer.from(secret, 'base64');
  const jwt = mintCdpJwt(cdpPrivateKey(secret), 'key-id', 'POST', '/settle', 1_700_000_000);
  const [header, claims, signature] = jwt.split('.');

  const pub = createPublicKey({
    key: { kty: 'OKP', crv: 'Ed25519', x: raw.subarray(32).toString('base64url') },
    format: 'jwk',
  });
  assert.equal(
    edVerify(null, Buffer.from(`${header}.${claims}`), pub, Buffer.from(signature, 'base64url')),
    true,
  );
  assert.equal(decode(header).alg, 'EdDSA');
  assert.equal(decode(claims).exp, 1_700_000_120);
});

test('reads the sponsor for the exact network and protocol version in use', async () => {
  const kinds = [
    { scheme: 'exact', network: 'solana', x402Version: 1, extra: { feePayer: 'v1-sponsor' } },
    { scheme: 'upto', network: 'solana:main', x402Version: 2, extra: { feePayer: 'wrong-scheme' } },
    { scheme: 'exact', network: 'solana:main', x402Version: 2, extra: { feePayer: 'right' } },
  ];
  const fetchImpl = (async () =>
    new Response(JSON.stringify({ kinds }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })) as unknown as typeof fetch;

  assert.equal(
    await cdpFeePayer('key-id', testSecret(), 'solana:main', fetchImpl),
    'right',
  );
});

test('reports no sponsor when the network is unsupported', async () => {
  const fetchImpl = (async () =>
    new Response(JSON.stringify({ kinds: [] }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })) as unknown as typeof fetch;

  assert.equal(await cdpFeePayer('key-id', testSecret(), 'solana:main', fetchImpl), undefined);
});
