import * as ed25519 from '@noble/ed25519';
import { sha512 } from '@noble/hashes/sha2.js';
import bs58 from 'bs58';

ed25519.hashes.sha512 = sha512;

const deriveBrokerKey = ed25519.getPublicKeyAsync;

export type AttestationPayload = {
  agent_did: string;
  provider: 'ionet' | 'akash';
  lease_id: string;
  gpu_hours: number;
  expires_at: number;
};

export function canonicalAttestation(p: AttestationPayload): Uint8Array {
  const fixed = {
    agent_did: p.agent_did,
    provider: p.provider,
    lease_id: p.lease_id,
    gpu_hours: p.gpu_hours,
    expires_at: p.expires_at,
  };
  return new TextEncoder().encode(JSON.stringify(fixed));
}

export async function sign(
  p: AttestationPayload,
  keyBytes: Uint8Array,
): Promise<{ signatureBs58: string; pubkeyBs58: string }> {
  if (keyBytes.length !== 32) throw new Error('broker key must be 32 bytes');
  const pk = await deriveBrokerKey(keyBytes);
  const sig = await ed25519.signAsync(canonicalAttestation(p), keyBytes);
  return { signatureBs58: bs58.encode(sig), pubkeyBs58: bs58.encode(pk) };
}

export async function verify(
  p: AttestationPayload,
  signatureBs58: string,
  pubkeyBs58: string,
): Promise<boolean> {
  try {
    const sig = bs58.decode(signatureBs58);
    const pk = bs58.decode(pubkeyBs58);
    if (sig.length !== 64 || pk.length !== 32) return false;
    return await ed25519.verifyAsync(sig, canonicalAttestation(p), pk);
  } catch {
    return false;
  }
}

export function hexToKey(hex: string): Uint8Array {
  const clean = hex.startsWith('0x') ? hex.slice(2) : hex;
  if (clean.length !== 64) throw new Error('broker key hex must be 64 chars');
  // parseInt yields NaN for a non-hex pair, which Uint8Array silently coerces
  // to 0 — a typo'd key would boot a broker with a wrong-but-valid identity.
  // Reject non-hex input loudly instead.
  if (/[^0-9a-fA-F]/.test(clean)) throw new Error('broker key hex must be valid hex');
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  return out;
}
