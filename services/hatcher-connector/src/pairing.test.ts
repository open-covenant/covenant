import { describe, it, expect } from 'vitest';
import { pairingMessage, proveIdentity, base58Encode, type Attestor } from './pairing.js';

describe('base58Encode', () => {
  it('matches known vectors and preserves leading zeros as leading 1s', () => {
    expect(base58Encode(new TextEncoder().encode('hello world'))).toBe('StV1DL6CwTryKyV');
    expect(base58Encode(new Uint8Array([0, 0, 1]))).toBe('112');
    expect(base58Encode(new Uint8Array([]))).toBe('');
  });
});

describe('pairingMessage (leg-3 anti-spoof framing)', () => {
  it('frames nonce|code|pubkey with 0x1f separators so a shifted boundary cannot spoof another split', () => {
    // Frozen against an independent base58 of "N\x1fC\x1fPK"; separator drift or
    // field reordering breaks this instead of silently passing a relational check.
    expect(pairingMessage('N', 'C', 'PK')).toBe('fuM82nXL');
    // The separators are load-bearing: moving the boundary between fields must
    // change the signed message, else a crafted nonce could absorb the code/pubkey.
    const canonical = pairingMessage('N', 'C', 'PK');
    expect(pairingMessage('NC', '', 'PK')).not.toBe(canonical);
    expect(pairingMessage('N', 'CP', 'K')).not.toBe(canonical);
  });
});

describe('proveIdentity (pairing leg-3)', () => {
  it('builds nonce‖code‖pubkey, signs it via the daemon, and returns the attestation', async () => {
    const calls: Array<{ m: string; ts: number }> = [];
    const attestor: Attestor = {
      async signAttestation(m, ts) {
        calls.push({ m, ts });
        return { signature_b58: 'SIG', pubkey_b58: 'PK', ts };
      },
    };

    const att = await proveIdentity(attestor, { nonce: 'N', pairingCode: 'C', daemonPubkeyB58: 'PK', now: () => 1234 });

    expect(att).toEqual({ signature_b58: 'SIG', pubkey_b58: 'PK', ts: 1234 });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.ts).toBe(1234);
    expect(calls[0]!.m).toBe(pairingMessage('N', 'C', 'PK'));
  });
});
