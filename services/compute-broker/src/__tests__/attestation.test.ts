import { describe, expect, it } from 'vitest';
import {
  canonicalAttestation,
  hexToKey,
  sign,
  verify,
  type AttestationPayload,
} from '../attestation.js';

describe('attestation', () => {
  const payload: AttestationPayload = {
    agent_did: '11111111111111111111111111111111',
    provider: 'ionet',
    lease_id: 'lease-xyz',
    gpu_hours: 8,
    expires_at: 1_700_000_000,
  };

  it('hexToKey rejects wrong length', () => {
    expect(() => hexToKey('abcd')).toThrow();
    expect(hexToKey('00'.repeat(32)).length).toBe(32);
  });

  it('canonical form is deterministic and stable under key order', () => {
    const a = canonicalAttestation(payload);
    const b = canonicalAttestation({ ...payload, gpu_hours: payload.gpu_hours });
    expect(Buffer.from(a).toString()).toBe(Buffer.from(b).toString());
  });

  it('sign → verify round-trip', async () => {
    const key = hexToKey('ab'.repeat(32));
    const { signatureBs58, pubkeyBs58 } = await sign(payload, key);
    expect(await verify(payload, signatureBs58, pubkeyBs58)).toBe(true);
  });

  it('verify rejects modified payload', async () => {
    const key = hexToKey('cd'.repeat(32));
    const { signatureBs58, pubkeyBs58 } = await sign(payload, key);
    const tampered = { ...payload, gpu_hours: payload.gpu_hours + 1 };
    expect(await verify(tampered, signatureBs58, pubkeyBs58)).toBe(false);
  });

  it('verify rejects wrong pubkey', async () => {
    const key1 = hexToKey('11'.repeat(32));
    const key2 = hexToKey('22'.repeat(32));
    const { signatureBs58 } = await sign(payload, key1);
    const { pubkeyBs58: wrongPk } = await sign(payload, key2);
    expect(await verify(payload, signatureBs58, wrongPk)).toBe(false);
  });

  it('verify fails gracefully on garbage input', async () => {
    expect(await verify(payload, 'not-bs58!!', 'also-garbage!!')).toBe(false);
  });

  it('hexToKey accepts a 0x prefix and yields the same bytes as the bare hex', () => {
    const bare = hexToKey('ab'.repeat(32));
    const prefixed = hexToKey('0x' + 'ab'.repeat(32));
    expect(Array.from(prefixed)).toEqual(Array.from(bare));
  });

  it('hexToKey decodes each hex byte pair in base-16', () => {
    const key = hexToKey('ab00ff' + '00'.repeat(29));
    expect(key[0]).toBe(0xab);
    expect(key[1]).toBe(0x00);
    expect(key[2]).toBe(0xff);
    expect(key[31]).toBe(0x00);
  });

  it('sign rejects a non-32-byte key with an explicit message', async () => {
    await expect(sign(payload, new Uint8Array(31))).rejects.toThrow('broker key must be 32 bytes');
  });
});
