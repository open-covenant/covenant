// Pairing leg-3 (PAIR PROVE): the connector proves the local daemon's ed25519
// identity to Hatcher without putting a shared secret on the wire. Hatcher sends a
// nonce; the connector asks the daemon to sign (nonce ‖ pairing_code ‖ daemon_pubkey)
// via POST /identity/sign (gated by the identity.attest capability), and returns the
// attestation. Hatcher verifies the signature against daemon_pubkey, recording it as
// the runner identity. Legs 1-2 (mint a scoped connector peer; claim a pairing code)
// are an operator/CLI step — see docs/hatcher-handoff.md §2.

import type { Attestation } from './daemon.js';

export interface Attestor {
  signAttestation(messageB58: string, ts: number): Promise<Attestation>;
}

const B58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

export function base58Encode(bytes: Uint8Array): string {
  if (bytes.length === 0) return '';
  const digits = [0];
  for (const byte of bytes) {
    let carry = byte;
    for (let j = 0; j < digits.length; j++) {
      carry += digits[j]! << 8;
      digits[j] = carry % 58;
      carry = (carry / 58) | 0;
    }
    while (carry > 0) {
      digits.push(carry % 58);
      carry = (carry / 58) | 0;
    }
  }
  let out = '';
  for (let k = 0; k < bytes.length && bytes[k] === 0; k++) out += '1';
  for (let q = digits.length - 1; q >= 0; q--) out += B58_ALPHABET[digits[q]!];
  return out;
}

// The canonical leg-3 message: nonce ‖ 0x1f ‖ pairing_code ‖ 0x1f ‖ daemon_pubkey_b58,
// base58-encoded for the daemon's message_b58 field. The 0x1f unit separators stop a
// crafted nonce from spoofing the code/pubkey boundary.
export function pairingMessage(nonce: string, pairingCode: string, daemonPubkeyB58: string): string {
  return base58Encode(new TextEncoder().encode(`${nonce}\x1f${pairingCode}\x1f${daemonPubkeyB58}`));
}

export async function proveIdentity(
  attestor: Attestor,
  args: { nonce: string; pairingCode: string; daemonPubkeyB58: string; now?: () => number },
): Promise<Attestation> {
  const messageB58 = pairingMessage(args.nonce, args.pairingCode, args.daemonPubkeyB58);
  const ts = (args.now ?? (() => Date.now()))();
  return attestor.signAttestation(messageB58, ts);
}
