# MagicBlock integration

Covenant exposes optional evidence and policy components for work routed through
MagicBlock ephemeral rollups (ERs). An operator can bond stake on L1 and submit
receipt hashes to an onchain accumulator. A Covenant-operated monitor can also
publish a signed statement about a TDX quote for an ER validator. These paths do
not mediate every action, prove the meaning or completeness of submitted
receipts, or independently establish that agent work ran inside the enclave.
Covenant runs alongside execution and does not hold the user's payment keys.

Observed on mainnet; the settlement program has a public verified-build record:

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
   └─ quote → issuer-authored ER statement (SAS), keyed to the validator,
              published after the monitor's DCAP verification path runs
```

Per-action state (counters, provenance roots) lives in the ER, where it is hot and
non-custodial. Custody, staking, slashing, and treasury stay on L1.

## 1. Read Covenant's ER observation

The Magic Router returns a set of ERs, each identified by a validator pubkey.
Covenant's monitor requests and locally DCAP-checks a quote from each configured
ER, then writes an issuer-authored `er-verified` credential to the Solana
Attestation Service, keyed to the router's validator address
([`services/er-registry`](../services/er-registry)). A resolver can use that
credential as a routing-policy input. The credential authenticates Covenant as
issuer; an account read does not independently reproduce the DCAP verification.

```js
import { Connection } from "@solana/web3.js";
import { pickVerifiedEr } from "@covenant-org/verified-er";

const { picked, routes } = await pickVerifiedEr(new Connection(RPC));
// picked.fqdn      -> an ER endpoint selected under the resolver policy
// picked.covenant  -> { status: "UpToDate", mrTd, verifiedAt, expiry, ... }
```

The credential carries the monitor's DCAP result (TCB status, `mr_td`) and is
signed by an authorized Covenant credential signer. Credentials expire after
72 hours; the resolver rejects missing, expired, wrong-issuer, and unacceptable
status records. Consumers still trust the monitor's implementation, issuer key,
quote endpoint binding, configuration, and renewal process. The resolver is
[`@covenant-org/verified-er`](../packages/verified-er) (read-only, one
dependency); another SAS client can inspect the same issuer-authored bytes.

## 2. Meter work into an on-chain provenance root

A credit account carries a `provenance_root`. Each successful
`consume_credits(amount, receipt_hash)` folds the caller-supplied receipt hash
into a hash-chain while the account is delegated to the ER. The delegated state
is intended to commit to L1 on undelegation:

```
provenance_root = sha256(provenance_root || receipt_hash)   // genesis = 32 zero bytes
```

The fold is deterministic, so someone with the exact ordered receipt hashes can
recompute it and compare it with the observed account root. A match proves only
that those hashes were folded in that order. The program does not prove that the
bytes describe work, that an output was delivered, or that every runtime action
was submitted.

## 3. Bond an agent, slash against its record

```
register_agent(agent_key, metadata_hash, capability_hash)   // bond the identity
stake(amount, lock_until)                                   // slashable CVNT position
slash_for_actions(amount)                                   // reason = on-chain provenance_root
```

`slash_for_actions` reads its reason bytes from the agent's observed
`provenance_root`, via the seed-bound credit account (`[b"credits", operator]`).
The slash call cannot substitute a different reason directly, but the root was
built from caller-supplied hashes and does not validate their semantics.

## 4. Verify the enclave

The registry monitor's verification path is in the tree
([`services/er-registry/tee.mjs`](../services/er-registry/tee.mjs)): pull a TDX
quote from the ER against a fresh 64-byte challenge, DCAP-verify it against the
Intel PCCS, enforce `report_data == challenge` so a stale or replayed quote
fails, and require TCB `UpToDate`. The result is what lands in the on-chain
attestation of section 1.

Still ahead: a `covenant-tee` crate that binds an agent plus its provenance root
into the quote challenge — a signed Covenant attestation tying the agent's
record to the enclave it ran in. The reference deployment below exercises the ER
metering and slashing loop; agent-bound enclave attestation is not part of it yet.

## Reference deployment

A historical demo submitted hashes associated with five prompt/answer pairs to
`mainnet-tee`. The observed credit-account root matched the supplied sequence,
and a 1000 CVNT slash was recorded against a 5000 CVNT stake. This demonstrates
the accumulator and slash path for those supplied hashes; it does not prove that
the runtime mediated the prompts, that the answers came from the named model, or
that the outputs were correct.

| What | Value |
|---|---|
| Metered credit account | `DrawYGmdbQ7sULxzzczUqyZT2nmP8SZeYPuJzy6TNksj` |
| Observed accumulator root | `2769ee46c8c7dc49e38737c8a3c6d0f57a48553d9b2af08d0bc82cf80ce88933` |
| Demo agent record (PDA) | `G2bMkQkGXTPv2rDLZpXqbn5fAehLqKujXWidcJbYHPwj` |
| ER validator subject | `MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo` (mainnet-tee) |

The reproducible driver is in `examples/magicblock/verify.mjs`; the ER instructions
themselves (`consume_credits`, `provenance_root`, `slash_for_actions`) live in
`agent-os/programs/settlement/src/lib.rs`.

## Use it

Beyond checking Covenant's own claims, these are capabilities any MagicBlock
builder or agent can consume directly:

- **[`@covenant-org/verified-er`](../packages/verified-er)** — select an ER under
  the package's issuer, expiry, and TCB-status policy and compare accumulator
  roots. Read-only, one dependency.
- **[`@covenant-org/er-guard`](../packages/er-guard)** — session-reliability
  keeper: cooperatively undelegates your accounts on idle, max-lifetime, or a
  validator stall, and documents what dlp 3.1.0's permissionless
  `RequestUndelegation` actually requires from your program.
- **Trust MCP** (`https://mcp.opencovenant.org/mcp`) — zero-install for any MCP
  agent: `covenant_verified_ers`, `covenant_verify_enclave` (live DCAP check),
  `covenant_er_provenance`.
- **Paid enclave verification** —
  `GET https://covenant-x402-seller.onrender.com/x402/er/enclave/{validator}`
  (x402, USDC on Solana): a fresh Covenant-signed DCAP verification, optionally
  binding an agent and its provenance root into the quote challenge.

## Try it

[`examples/magicblock`](../examples/magicblock) is a read-only mainnet demo that
reproduces the discovery and provenance checks above. It takes no keys and moves no
funds.

```
cd examples/magicblock && npm install && node verify.mjs
```

It resolves ERs with currently accepted Covenant credentials, then recomputes
the reference accumulator root from supplied demo records and compares it with
the observed account.

For integration help, open an issue or reach out at
[opencovenant.org/contact](https://opencovenant.org/contact).
