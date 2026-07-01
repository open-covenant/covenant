# MagicBlock integration

Covenant is a verifiable trust layer over MagicBlock's execution. MagicBlock runs
agent work fast and private in ephemeral rollups (ERs); Covenant makes that work
accountable. An agent bonds a slashable stake, every metered action folds into an
on-chain provenance root, and any client can check that the rollup it runs on is a
genuine, attested TDX enclave. Covenant sits alongside the execution layer and
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
MagicBlock ER  ── executes agent work (fast, gasless, optionally private/TEE)
   │
   ├─ meter → consume_credits folds a receipt hash-chain into the credit
   │          account's provenance_root, gaslessly, committed to L1 on undelegate
   ├─ bond  → register_agent + stake (CVNT); slash_for_actions burns the bond
   │          with the reason read from the on-chain provenance_root
   └─ trust → verified-ER attestation (SAS) keyed to the validator identity;
              covenant-tee verifies the Private ER's TDX enclave via DCAP
```

The split is deliberate. Hot, non-custodial per-action state (counters, provenance
roots) lives in the ER. Custody, staking, slashing, and treasury stay on L1 and
never move.

## 1. Discover a Covenant-verified ER

The Magic Router returns a set of ERs, each identified by a validator pubkey.
Covenant publishes a verified-ER attestation through the Solana Attestation
Service, keyed to that validator identity. An agent picking where to run resolves
the trust signal in one account read. No router change, no indexer.

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
signed by the Covenant issuer. A relying party trusts it by checking the signer is
an authorized signer of the credential. Validator identities do not rotate, so the
key is stable.

## 2. Meter work into an on-chain provenance root

A credit account carries a `provenance_root`. Each `consume_credits(amount,
receipt_hash)` folds the receipt into a hash-chain, gaslessly while the account is
delegated to the ER, and commits to L1 on undelegate:

```
provenance_root = sha256(provenance_root || receipt_hash)   // genesis = 32 zero bytes
```

Because the fold is deterministic, anyone can recompute it from the receipts and
check it against the on-chain root. If a single action is altered, added, or
dropped, the roots diverge. The receipt is yours to define: hash the actual work
product (the intent and the agent's output) and the on-chain record *is* the work.

## 3. Bond an agent, slash against its record

```
register_agent(agent_key, metadata_hash, capability_hash)   // bond the identity
stake(amount, lock_until)                                   // slashable CVNT position
slash_for_actions(amount)                                   // reason = on-chain provenance_root
```

`slash_for_actions` reads the reason straight from the agent's on-chain
`provenance_root`, via the seed-bound credit account (`[b"credits", operator]`).
There is no caller-supplied reason to forge; the penalty is anchored to the
verifiable record of what the agent actually did.

## 4. Verify the enclave

The `covenant-tee` crate (`agent-os/crates/covenant-tee`) pulls a TDX quote from a
MagicBlock Private ER, verifies it with Intel DCAP against the Phala PCCS, and
binds an agent plus its provenance root into the 64-byte quote challenge. The
result is a signed Covenant attestation proving a specific agent's record came
from a genuine, attested enclave. This is the same verification behind the
verified-ER lookup in section 1.

## Reference deployment

Proven end to end with our own agent. The Covenant demo agent (Haiku 4.5) answered
five real prompts; each answer was metered on the verified ER (`mainnet-tee`), and
the credit account's on-chain `provenance_root` equals the hash-chain of those
exact answers. The agent is bonded with a 5000 CVNT stake, of which 1000 was
slashed against that record.

| What | Value |
|---|---|
| Metered credit account | `DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj` |
| Provenance root (of the real work) | `2769ee46c8c7dc49e38737c8a3c6d0f57a48553d9b2af08d0bc82cf80ce88933` |
| Agent identity (PDA) | `G2bMkQkGXTPv2rDLZpXqbn5fAehLqKujXWidcJbYHPwj` |
| Verified ER validator | `MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo` (mainnet-tee) |

The drivers are in `agent-os/programs/settlement-ephemeral/spike/`:
`reference-run.mjs` (agent work + provenance), `bond-slash.mjs` (bond + slash),
`er-registry.mjs` and `pick-verified-er.mjs` (SAS attest, resolve, discover).

Building an agent on MagicBlock and want verifiable bonds, slashing, or the
verified-ER lookup wired in? Open an issue or reach out at
[opencovenant.org/contact](https://opencovenant.org/contact).
