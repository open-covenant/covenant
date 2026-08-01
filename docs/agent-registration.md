# Agent registration document

A Covenant agent can generate one self-authored JSON document that matches the
pinned historical **A2A AgentCard** and **ERC-8004 registration-file** shapes
used by this implementation. It can advertise endpoints and registration
identifiers to compatible readers. It does not make those claims true, prove
who operates the agent, or give a client a reason to trust it.

The builder lives in `covenant-identity` (`src/registration.rs`) as
[`AgentRegistration`]. It is derived from the agent's canonical ed25519 identity
used for Covenant-local protocol statements. Solana and EVM payment funding keys
are separate and remain in their signer paths. A valid self-signature
authenticates the key that authored the document; it does not independently bind
that key to a person, organization, capability, endpoint, or onchain
registration.

## Pinned dual-schema shape

At the pinned commits below, the A2A AgentCard schema does not forbid extra properties, and every ERC-8004
registration field except `supportedTrust` is mandatory. So a document that
carries the **union** of both schemas' required fields validates as both: an A2A
client can read the A2A fields and ignore the rest; an ERC-8004 client can read
the ERC-8004 fields and ignore the rest. This is a fixture compatibility result,
not a guarantee for later revisions of either specification.

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

`agentId` is `0` until the agent is registered onchain; the caller supplies the
later `agentId` and `<CovenantRegistry>` address. A consumer must verify those
values against the named registry rather than trusting the document.

## Signing

The document is signed with a detached JWS (RFC 7515, `alg=EdDSA`) over the
RFC 8785 JCS canonicalization of the body with the `signatures` array removed.
The signature is stored in the A2A `signatures` array. A consumer with the
expected public key can verify authorship and detect changed bytes. JCS gives a
deterministic byte string; it does not validate the document's claims.

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

## Scope

Generation, self-signing, and verification against an explicitly supplied key
are **local only** and implemented today.
Publishing the document to a public `/.well-known/agent-registration.json`
endpoint or registering the agent in an on-chain ERC-8004 registry is **planned**
and tracked under the multichain roadmap (registry deployment is operator-gated).

[`AgentRegistration`]: ../agent-os/crates/covenant-identity/src/registration.rs
