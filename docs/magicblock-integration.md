# MagicBlock integration

Covenant adds accountability to agent work that runs in MagicBlock ephemeral
rollups (ERs). An agent bonds a slashable stake on L1, every metered action folds
into an on-chain provenance root, and a client can check that the ER it runs on is
a TDX enclave with a valid attestation. Covenant runs alongside execution and
never touches custody or payments.

Live on mainnet, OtterSec-verified:

| What | Address |
|---|---|
| Settlement program | `cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y` |
| Attestation service (SAS) | `22zoJMtdu4tQc2PzL74ZUT7FrwgB1Udec8DdW4yw4BdG` |
| Covenant issuer (credential authority) | `AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb` |
| Credential / schema | `covenant` / `er-verified` v1 |

Verify the deployed bytecode against the source at
[verify.osec.io](https://verify.osec.io/status/cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y).

## Architecture

```
MagicBlock ER  ── runs agent work (gasless, optionally private/TEE)
   │
   ├─ meter → consume_credits folds a receipt hash-chain into the credit
   │          account's provenance_root, gaslessly, committed to L1 on undelegate
   ├─ bond  → register_agent + stake (CVNT); slash_for_actions burns the bond
   │          with the reason read from the on-chain provenance_root
   └─ trust → verified-ER attestation (SAS) keyed to the validator identity;
              enclave (TDX/DCAP) verification is a planned addition
```

Per-action state (counters, provenance roots) lives in the ER, where it is hot and
non-custodial. Custody, staking, slashing, and treasury stay on L1.

## 1. Discover a Covenant-verified ER

The Magic Router returns a set of ERs, each identified by a validator pubkey.
Covenant publishes a verified-ER attestation through the Solana Attestation
Service, keyed to that validator identity. An agent picking where to run resolves
it in one account read. No router change, no indexer.

```js
import { deriveCredentialPda, deriveSchemaPda, deriveAttestationPda,
  fetchMaybeAttestation, fetchSchema, deserializeAttestationData } from "sas-lib";

const ISSUER = "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb"; // Covenant credential authority
const [credential] = await deriveCredentialPda({ authority: ISSUER, name: "covenant" });
const [schema]     = await deriveSchemaPda({ credential, name: "er-verified", version: 1 });

// validator = an ER identity from the router's getRoutes
const [attestation] = await deriveAttestationPda({ credential, schema, nonce: validator });
const acct = await fetchMaybeAttestation(rpc, attestation);

const verified = acct.exists &&
  deserializeAttestationData(await fetchSchema(rpc, schema), acct.data.data).verified;
```

The attestation carries the enclave's DCAP result (TCB status, `mr_td`) and is
signed by the Covenant issuer. A reader trusts it by checking the signer is an
authorized signer of the credential. Validator identities do not rotate, so the
key is stable.

## 2. Meter work into an on-chain provenance root

A credit account carries a `provenance_root`. Each `consume_credits(amount,
receipt_hash)` folds the receipt into a hash-chain, gaslessly while the account is
delegated to the ER, and commits to L1 on undelegate:

```
provenance_root = sha256(provenance_root || receipt_hash)   // genesis = 32 zero bytes
```

The fold is deterministic, so anyone can recompute it from the receipts and compare
against the on-chain root. Alter, add, or drop one action and the roots diverge.
The receipt is yours to define: hash the work product (the intent and the agent's
output) and the on-chain root becomes a record of what the agent did.

## 3. Bond an agent, slash against its record

```
register_agent(agent_key, metadata_hash, capability_hash)   // bond the identity
stake(amount, lock_until)                                   // slashable CVNT position
slash_for_actions(amount)                                   // reason = on-chain provenance_root
```

`slash_for_actions` reads the reason from the agent's on-chain `provenance_root`,
via the seed-bound credit account (`[b"credits", operator]`). There is no
caller-supplied reason to forge; the penalty is tied to the on-chain record.

## 4. Verify the enclave (planned)

Enclave-level verification is a planned addition, not yet in the tree: a
`covenant-tee` crate would pull a TDX quote from a MagicBlock Private ER, verify it
with Intel DCAP against the Phala PCCS, and bind an agent plus its provenance root
into the 64-byte quote challenge — a signed Covenant attestation tying the agent's
record to the enclave it ran in. The reference deployment below exercises the ER
metering and slashing loop; enclave attestation is not part of it yet.

## Reference deployment

Proven end to end with our own agent. The Covenant demo agent (Haiku 4.5) answered
five prompts; each answer was metered on the verified ER (`mainnet-tee`), and the
credit account's on-chain `provenance_root` equals the hash-chain of those exact
answers. The agent holds a 5000 CVNT stake, of which 1000 was slashed against that
record.

| What | Value |
|---|---|
| Metered credit account | `DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj` |
| Provenance root (of the real work) | `2769ee46c8c7dc49e38737c8a3c6d0f57a48553d9b2af08d0bc82cf80ce88933` |
| Agent identity (PDA) | `G2bMkQkGXTPv2rDLZpXqbn5fAehLqKujXWidcJbYHPwj` |
| Verified ER validator | `MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo` (mainnet-tee) |

The reproducible driver is in `examples/magicblock/verify.mjs`; the ER instructions
themselves (`consume_credits`, `provenance_root`, `slash_for_actions`) live in
`agent-os/programs/settlement/src/lib.rs`.

## Try it

[`examples/magicblock`](../examples/magicblock) is a read-only mainnet demo that
reproduces the discovery and provenance checks above. It takes no keys and moves no
funds.

```
cd examples/magicblock && npm install && node verify.mjs
```

It resolves which ERs are Covenant-verified from the router, then recomputes the
reference agent's provenance root from its answers and checks it against the chain.

For integration help, open an issue or reach out at
[opencovenant.org/contact](https://opencovenant.org/contact).
