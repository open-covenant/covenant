# @covenant-org/verified-er

Pick a Covenant-verified [MagicBlock](https://www.magicblock.xyz) ephemeral
rollup, and check an agent's on-chain provenance root. Read-only — no keys, no
funds, one dependency (`@solana/web3.js`).

Covenant continuously DCAP-verifies the TDX enclaves behind MagicBlock's ERs and
publishes the result as [Solana Attestation Service](https://attest.solana.com)
attestations keyed to the validator identity the Magic Router already returns.
Verification is one account read per validator. Attestations carry an expiry:
if the monitor stops re-verifying an enclave, its attestation lapses and this
library fails closed to unverified.

```
npm install @covenant-org/verified-er
```

## Pick where to run

```js
import { Connection } from "@solana/web3.js";
import { pickVerifiedEr } from "@covenant-org/verified-er";

const connection = new Connection("https://api.mainnet-beta.solana.com");
const { picked, routes } = await pickVerifiedEr(connection);

// picked.fqdn      -> the verified ER endpoint to send transactions to
// picked.covenant  -> { status: "UpToDate", mrTd: "...", verifiedAt, expiry, ... }
for (const r of routes) console.log(r.fqdn, r.covenant.verified, r.covenant.reason);
```

Or resolve a single validator you already have from `getRoutes`:

```js
import { resolveVerifiedEr } from "@covenant-org/verified-er";
const v = await resolveVerifiedEr(connection, "MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo");
```

## Check an agent's record

Every action a Covenant-metered agent takes folds a receipt hash into the
`provenance_root` on its credit account — updated gaslessly in the ER, committed
to L1. The fold is deterministic, so the record is checkable from the receipts.

A receipt hash is a 32-byte `sha256` you compute over whatever you defined the
action to be (the settlement publisher uses `sha256(intent_id)`). Pass them in
action order:

```js
import { verifyProvenanceRoot } from "@covenant-org/verified-er";
import { createHash } from "node:crypto";

const receiptHashes = actions.map((a) => createHash("sha256").update(a.intentId).digest());

const { match, onChain, recomputed } = await verifyProvenanceRoot(
  connection,
  "DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj",   // the agent's credit account
  receiptHashes,                                     // 32-byte Buffers or hex strings
);
```

`foldProvenance(receiptHashes)` returns the same root offline without a chain
read. Alter, add, or drop one action and the roots diverge.

## Trust model

An ER counts as verified when all three hold:

1. an attestation exists at the PDA derived from the validator identity,
2. its signer is an authorized signer of the Covenant credential
   (`AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb`), and
3. it has not expired.

The attestation records the enclave's DCAP result (TCB status, `mr_td`) and when
it was verified. Full integration guide:
[docs/magicblock-integration.md](https://github.com/open-covenant/covenant/blob/main/docs/magicblock-integration.md).
