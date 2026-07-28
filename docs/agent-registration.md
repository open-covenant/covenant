# Agent registration document

A Covenant agent can publish one JSON document that is simultaneously a valid
**A2A AgentCard** and an **ERC-8004 registration file**. A single document lets
any A2A-aware client, any EVM/ERC-8004 client, and any CAIP-aware wallet
discover and trust the same agent — without Covenant infrastructure in the trust
path, and without maintaining two divergent identity documents.

The builder lives in `covenant-identity` (`src/registration.rs`) as
[`AgentRegistration`]. It is derived from the agent's canonical ed25519 identity
— the same key used for local signing and on-chain settlement — so the document
needs no second keypair system.

## Why one document validates as both

The A2A AgentCard schema does not forbid extra properties, and every ERC-8004
registration field except `supportedTrust` is mandatory. So a document that
carries the **union** of both schemas' required fields validates as both: an A2A
client reads the A2A fields and ignores the rest; an ERC-8004 client reads the
ERC-8004 fields and ignores the rest.

| Field group | Members |
|---|---|
| A2A AgentCard (required) | `protocolVersion`, `name`, `description`, `url`, `version`, `capabilities`, `defaultInputModes`, `defaultOutputModes`, `skills` |
| ERC-8004 registration (required) | `type`, `name`, `description`, `image`, `services`, `x402Support`, `active`, `registrations` |
| ERC-8004 (optional) | `supportedTrust` |
| A2A signatures | `signatures` (RFC 7515 detached JWS) |

## Pinned spec commits

The ERC-8004 draft has churned the trust field name between `trustModels` and
`supportedTrust`, so field names are pinned to specific upstream commits and
asserted by a fixture test. A future spec revision surfaces as a failing test,
not a silently unparseable card.

| Spec | Source | Commit |
|---|---|---|
| ERC-8004 registration | `ethereum/ERCs` `ERCS/erc-8004.md` | `503591a6e80e6e1affdd6403341e25269141f046` |
| A2A AgentCard | `a2aproject/A2A` `specification/json/a2a.json` (v0.3.0) | `210f03d426e2f2fa92000e14ef0de3b7ba15aee5` |

At the pinned ERC-8004 commit the trust field is `supportedTrust` and the
registration discriminator is
`type: "https://eips.ethereum.org/EIPS/eip-8004#registration-v1"`.

## Home registry in CAIP-2 form

The home registry is expressed in the CAIP-2 form EVM/CAIP-aware clients already
parse — `{namespace}:{chainId}:{registry}` — using the Solana mainnet-beta
genesis hash rather than a network name:

```
"registrations": [
  { "agentId": 0, "agentRegistry": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:<CovenantRegistry>" }
]
```

`solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` is the `solana` namespace plus the
mainnet-beta genesis hash truncated to 32 characters, per the ChainAgnostic
Solana namespace definition. The document also carries a `did:pkh` service entry
bound to the agent's Solana address (its ed25519 public key):

```
{ "name": "DID", "endpoint": "did:pkh:solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:<pubkey>", "version": "v1" }
```

`agentId` is `0` when the registry mints no numeric id — a Solana home registry
identifies the agent by its pubkey, the `did:pkh` subject — and carries the
ERC-721 tokenId for registries that do (see the published card below).

## Signing

The document is signed with a detached JWS (RFC 7515, `alg=EdDSA`) over the
RFC 8785 JCS canonicalization of the body with the `signatures` array removed.
The signature is stored in the A2A `signatures` array, so it is A2A-native and
verifiable by any RFC 7515 consumer. JCS gives a deterministic byte string, so
independent signers and verifiers agree on the signed bytes.

```rust
use covenant_identity::{LocalIdentity, RegistrationParams};

let id = LocalIdentity::generate("agent@local");
let params = RegistrationParams::solana_mainnet(
    "covenant-agent",
    "governed, accountable execution",
    "https://covenant.example/agent.png", // image
    "https://covenant.example/a2a",       // A2A url
    "0.1.0",                               // version
    "<CovenantRegistry>",                  // on-chain registry address
);
let doc = id.signed_registration_document(&params)?;

// Verify under the agent's own ed25519 key.
let vk = covenant_identity::verifying_key_from_bytes(id.pubkey_bytes())?;
doc.verify(&vk)?;
```

## The published Covenant Foundation card

The Covenant Foundation agent is registered in the ERC-8004 IdentityRegistry on
Base mainnet (`agentId 58403`, registry `0x8004A169…a432`; see
`agent-os/evm/deployments.json`), and that registration's `agentURI` serves
`https://opencovenant.org/agents/covenant-foundation.json`.

The canonical content of that document is produced by
`covenant_identity::covenant_foundation_card()` and emitted by:

```bash
cd agent-os && cargo run -p covenant-identity --example generate_agent_card
```

The card preserves the agent's identity across both chains: the Solana
identity pubkey as the `did:pkh` service subject, the MPL Core home registry
in CAIP-2 genesis form, and the Base ERC-721 registration as a second
`registrations` entry:

```
"registrations": [
  { "agentId": 0, "agentRegistry": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp:CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d" },
  { "agentId": 58403, "agentRegistry": "eip155:8453:0x8004A169FB4a3325136EB29fA0ceB6D2e539a432" }
]
```

The home entry keeps `agentId: 0`: MPL Core mints no numeric tokenId, so the
Solana identity is keyed by the agent's pubkey (the `did:pkh` subject), and the
entry is a Covenant convention pointing at the home registry rather than an
on-chain ERC-8004 registration. The Base entry is the on-chain ERC-721
registration.

A golden fixture
(`agent-os/crates/covenant-identity/tests/fixtures/covenant-foundation.unsigned.json`)
pins the generator's output byte-for-byte in the crate's tests, and
`agent-os/scripts/validate-agentcard-conformance.mjs` (run by `validate.sh`)
checks both the fixture and the live served file against the dual-shape rules,
reporting any divergence field by field. Because the registered `agentURI`
serves whatever sits at that path, replacing the live file — and signing the
card with the Foundation identity key (`signatures`) — is a deliberate,
reviewed release step, not an automated write. That review also covers the
assertions the regenerated card adds over the previously served file
(`x402Support`, `supportedTrust`).

## Scope

Generation, signing, and verification are **local only**. Signing the
published card and replacing the served file are operator steps; registering
in further on-chain registries remains tracked under the multichain roadmap.

[`AgentRegistration`]: ../agent-os/crates/covenant-identity/src/registration.rs
