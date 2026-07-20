# covenant-blockrun-verify

A paid, public endpoint that verifies a [BlockRun](https://blockrun.ai) call
receipt — Covenant dogfooding x402, a trust service on the same rail the receipts
it checks were settled over.

An agent or organization pays USDC over x402 and gets back a **Covenant-signed
statement** about their receipt:

- its RFC 8785 (JCS) SHA-256 **digest**,
- whether the stated **verdict** matches the requested-vs-served model pair
  (`delivered` / `substituted` / `unverified`),
- whether it carries a **settlement** tx,

all wrapped in an ed25519 attestation that verifies against a published pubkey
with no trust in the server.

## Endpoint

```
POST /x402/verify
{ "receipt": { … }, "expectedDigest": "…"? }
→ { "result": { valid, digest, verdictConsistent, expectedVerdict, settled }, "attestation": { … } }
```

Price defaults to 0.005 USDC (`BLOCKRUN_VERIFY_PRICE`, base units). The x402
middleware fails closed: a failed settlement or a malformed receipt (>= 400) is
never charged. Discovery metadata is at `GET /.well-known/x402`.

## Why it's trustworthy

The digest is byte-identical to the `covenant-blockrun` Rust crate,
`@covenant-org/blockrun` (TS), and `covenant-blockrun` (Python) — a receipt built
by any of them verifies here, guarded by a shared cross-language digest test. The
verification result is signed, so the buyer pins the pubkey and checks the
statement themselves; the server is not trusted.

## Run

```bash
npm install && npm run build && npm start
```

Configuration mirrors the Covenant x402 seller (`X402_NETWORK`,
`COVENANT_BASE_PAYTO`, CDP credentials for Base mainnet settlement,
`COVENANT_ATTEST_KEYPAIR`). See `render.yaml` and `src/server.ts`.

Apache-2.0.
