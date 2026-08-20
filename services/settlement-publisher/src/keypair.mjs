// Pure validation + decode of the operator keypair env var, extracted from
// index.mjs so it is unit-testable without booting the sidecar (importing
// index.mjs starts a Connection, Anchor provider, HTTP server, and poll
// loop). This module has no @solana/web3.js import; index.mjs wraps the
// returned bytes in Keypair.fromSecretKey.
const ENV = "COVENANT_OPERATOR_KEYPAIR_JSON";

export function parseOperatorKeypairBytes() {
  const raw = process.env[ENV]?.trim();
  if (!raw) {
    throw new Error(`${ENV} is not set; sidecar can't sign`);
  }
  const arr = JSON.parse(raw);
  if (!Array.isArray(arr) || arr.length !== 64) {
    throw new Error(`${ENV} must be a 64-byte JSON array`);
  }
  return Uint8Array.from(arr);
}
