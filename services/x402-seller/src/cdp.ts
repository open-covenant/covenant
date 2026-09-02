/**
 * Settling through Coinbase's facilitator instead of PayAI's.
 *
 * The only reason to hand settlement to someone else is discovery. The x402
 * Bazaar, which agentic.market mirrors, builds its index from payments that
 * Coinbase's own facilitator settles. A resource that settles anywhere else is
 * invisible there no matter how correct its discovery metadata is, which is why
 * these routes advertised a valid bazaar extension for weeks and never appeared.
 *
 * This supplies only authentication. `HTTPFacilitatorClient` already speaks the
 * protocol, and CDP implements the same verify/settle surface, so nothing else
 * about the seller needs to know which facilitator it is talking to.
 */

import { createPrivateKey, randomBytes, sign, type KeyObject } from 'node:crypto';

export const CDP_HOST = 'api.cdp.coinbase.com';
export const CDP_FACILITATOR_URL = `https://${CDP_HOST}/platform/v2/x402`;

const b64url = (value: Buffer | string) => Buffer.from(value).toString('base64url');

/**
 * CDP issues the Ed25519 seed and public key base64-encoded and concatenated.
 * Node accepts that as a JWK, which is the least fragile route in without
 * hand-assembling PKCS#8.
 */
export function cdpPrivateKey(secret: string): KeyObject {
  const raw = Buffer.from(secret, 'base64');
  if (raw.length !== 64) {
    throw new Error(`CDP_API_KEY_SECRET decodes to ${raw.length} bytes, expected 64`);
  }
  return createPrivateKey({
    key: {
      kty: 'OKP',
      crv: 'Ed25519',
      d: b64url(raw.subarray(0, 32)),
      x: b64url(raw.subarray(32)),
    },
    format: 'jwk',
  });
}

/**
 * A bearer token for exactly one request. CDP binds the token to the method and
 * path through the `uris` claim, so a token minted for `/verify` is refused at
 * `/settle`. That is why the facilitator client's auth hook is keyed by path
 * rather than returning one header for everything.
 */
export function mintCdpJwt(
  key: KeyObject,
  keyId: string,
  method: string,
  path: string,
  now: number = Math.floor(Date.now() / 1000),
): string {
  const header = b64url(
    JSON.stringify({ typ: 'JWT', alg: 'EdDSA', kid: keyId, nonce: randomBytes(16).toString('hex') }),
  );
  const claims = b64url(
    JSON.stringify({
      sub: keyId,
      iss: 'cdp',
      aud: ['cdp_service'],
      nbf: now,
      exp: now + 120,
      uris: [`${method} ${CDP_HOST}/platform/v2/x402${path}`],
    }),
  );
  const signing = `${header}.${claims}`;
  return `${signing}.${b64url(sign(null, Buffer.from(signing), key))}`;
}

export interface CdpAuthHeaders {
  verify: Record<string, string>;
  settle: Record<string, string>;
  supported: Record<string, string>;
  bazaar?: Record<string, string>;
}

/**
 * Headers keyed by facilitator path, which is the shape the client expects. A
 * flat headers object is silently ignored.
 */
export function cdpAuthHeaders(keyId: string, secret: string): () => Promise<CdpAuthHeaders> {
  const key = cdpPrivateKey(secret);
  const bearer = (method: string, path: string) => ({
    Authorization: `Bearer ${mintCdpJwt(key, keyId, method, path)}`,
  });
  return async () => ({
    verify: bearer('POST', '/verify'),
    settle: bearer('POST', '/settle'),
    supported: bearer('GET', '/supported'),
    bazaar: bearer('GET', '/discovery/resources'),
  });
}

/**
 * The fee payer CDP will sponsor for a network, read from its own `/supported`.
 *
 * The challenge has to advertise the facilitator's fee payer, because the payer
 * builds the transaction around it. Advertising PayAI's key while settling
 * through Coinbase produces a transaction Coinbase will not sign for.
 */
export async function cdpFeePayer(
  keyId: string,
  secret: string,
  network: string,
  fetchImpl: typeof fetch = fetch,
  timeoutMs = 15_000,
): Promise<string | undefined> {
  const key = cdpPrivateKey(secret);
  const response = await fetchImpl(`${CDP_FACILITATOR_URL}/supported`, {
    headers: { Authorization: `Bearer ${mintCdpJwt(key, keyId, 'GET', '/supported')}` },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!response.ok) throw new Error(`CDP /supported failed: ${response.status}`);
  const body = (await response.json()) as {
    kinds?: Array<{ scheme?: string; network?: string; x402Version?: number; extra?: unknown }>;
  };
  const match = body.kinds?.find(
    (kind) => kind.scheme === 'exact' && kind.network === network && kind.x402Version === 2,
  );
  const extra = match?.extra;
  if (!extra || typeof extra !== 'object') return undefined;
  const feePayer = (extra as { feePayer?: unknown }).feePayer;
  return typeof feePayer === 'string' && feePayer.length > 0 ? feePayer : undefined;
}
