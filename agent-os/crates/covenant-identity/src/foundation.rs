//! The Covenant Foundation agent's canonical registration card.
//!
//! The Base mainnet ERC-8004 registration (agentId 58403 in the
//! `0x8004A169…` IdentityRegistry, recorded in `agent-os/evm/deployments.json`)
//! points its `agentURI` at `https://opencovenant.org/agents/covenant-foundation.json`.
//! This module is the single producer of that document: the served card must
//! be this builder's output (plus operator-held signatures), never a
//! hand-written file, so the published identity can never drift from the
//! [`AgentRegistration`] shape the crate parses and signs.
//!
//! Identity values are preserved from the live registrations. The Solana
//! identity is the agent pubkey, carried as the `did:pkh` service subject;
//! the home registry is the MPL Core program in CAIP-2 genesis form (the
//! hand-written card said `solana:101:metaplex`, a network-name form no
//! CAIP-aware client resolves); the Base ERC-721 registration keeps its
//! numeric agentId. The golden fixture at
//! `tests/fixtures/covenant-foundation.unsigned.json` freezes the exact
//! bytes, so any content change is a deliberate, reviewed edit here.

use crate::registration::{
    AgentRegistration, Registration, RegistrationParams, Service, Skill, SOLANA_MAINNET_CAIP2,
};

/// The Foundation agent's Solana address (its ed25519 identity pubkey) —
/// the account its MPL Core identity and audit-root attestations hang off.
pub const FOUNDATION_PUBKEY_BASE58: &str = "4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc";

/// MPL Core, the program the Foundation agent's Solana identity asset lives
/// under — the home registry. Same id `covenant-metaplex` pins as
/// `MPL_CORE_PROGRAM_ID`.
pub const FOUNDATION_SOLANA_REGISTRY: &str = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";

/// The Base mainnet ERC-8004 IdentityRegistry holding the Foundation
/// agent's ERC-721 registration, and the agentId it minted (both recorded
/// under `covenantMainnet.erc8004Registration` in
/// `agent-os/evm/deployments.json`).
pub const FOUNDATION_BASE_REGISTRY: &str = "eip155:8453:0x8004A169FB4a3325136EB29fA0ceB6D2e539a432";
pub const FOUNDATION_BASE_AGENT_ID: u64 = 58_403;

const NAME: &str = "Covenant Foundation Agent";
const DESCRIPTION: &str = "The Covenant Foundation's production agent, running on covenantd, the open-source agent operating system for accountable autonomy. Every action it takes passes a scoped capability check, debits an enforced budget, and is recorded in an append-only, hash-chained audit log. Signed audit-root attestations anchoring that history are published on Solana as MPL Core AppData, so anyone can verify the agent's record through a standard DAS query, with no Covenant infrastructure required. Identity, permissions, and a tamper-evident audit trail: trust you can check, not claim.";
const IMAGE: &str = "https://opencovenant.org/agents/covenant-agent-1024.png";
const URL: &str = "https://opencovenant.org";
const VERSION: &str = "0.1.0";

fn skill(id: &str, name: &str, description: &str, tags: &[&str]) -> Skill {
    Skill {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        examples: Vec::new(),
    }
}

/// Build the unsigned Foundation card. Deterministic: every value is a
/// documented constant, so the output is byte-stable and the golden test
/// can pin it. Signing is a separate, operator-held step
/// ([`AgentRegistration::sign`] with the Foundation identity key).
pub fn covenant_foundation_card() -> AgentRegistration {
    // The hand-written card advertised these under services[].skills, a
    // field the ERC-8004 service entry does not have; they are A2A skills.
    let skills = [
        skill(
            "identity",
            "Agent identity",
            "Canonical ed25519 agent identity, anchored on Solana as an MPL Core asset.",
            &["identity", "solana"],
        ),
        skill(
            "capabilities",
            "Scoped capabilities",
            "Every action passes a scoped capability check with enforced budgets.",
            &["authorization", "governance"],
        ),
        skill(
            "audit-attestation",
            "Audit-root attestation",
            "Signed audit-root attestations over the append-only, hash-chained audit log, verifiable via a standard DAS query.",
            &["audit", "attestation", "solana"],
        ),
        skill(
            "operator-console",
            "Operator console",
            "Web console for operating the agent in the sandbox environment.",
            &["console"],
        ),
        skill(
            "mcp-tools",
            "MCP tools",
            "Daemon-mediated MCP tool execution under capability enforcement.",
            &["mcp", "tools"],
        ),
        skill(
            "audit-log",
            "Audit log",
            "Append-only, hash-chained record of every governed action.",
            &["audit"],
        ),
        skill(
            "documentation",
            "Documentation",
            "Covenant protocol and operator documentation.",
            &["docs"],
        ),
    ];
    let extra_services = [
        Service {
            name: "web".to_string(),
            endpoint: "https://opencovenant.org".to_string(),
            version: None,
        },
        Service {
            name: "web".to_string(),
            endpoint: "https://sandbox.opencovenant.org".to_string(),
            version: None,
        },
        Service {
            name: "web".to_string(),
            endpoint: "https://docs.opencovenant.org".to_string(),
            version: None,
        },
    ];
    // agentId 0 on the home entry: MPL Core mints no numeric tokenId — the
    // Solana identity is keyed by the pubkey, which rides the did:pkh
    // subject. The Base entry carries the real ERC-721 id.
    let extra_registrations = [Registration {
        agent_id: FOUNDATION_BASE_AGENT_ID,
        agent_registry: FOUNDATION_BASE_REGISTRY.to_string(),
    }];
    let params = RegistrationParams {
        name: NAME,
        description: DESCRIPTION,
        image: IMAGE,
        url: URL,
        version: VERSION,
        registry_caip2: SOLANA_MAINNET_CAIP2,
        registry_address: FOUNDATION_SOLANA_REGISTRY,
        agent_id: 0,
        x402_support: true,
        active: true,
        supported_trust: crate::registration::DEFAULT_SUPPORTED_TRUST,
        skills: &skills,
        extra_services: &extra_services,
        extra_registrations: &extra_registrations,
    };
    AgentRegistration::build(FOUNDATION_PUBKEY_BASE58, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    const GOLDEN: &str = include_str!("../tests/fixtures/covenant-foundation.unsigned.json");

    #[test]
    fn card_matches_the_golden_fixture_byte_for_byte() {
        // The fixture is the reviewed content the served card must carry;
        // regenerating it is a deliberate edit here, not drift.
        let rendered = serde_json::to_string_pretty(&covenant_foundation_card()).unwrap() + "\n";
        assert_eq!(rendered, GOLDEN);
    }

    #[test]
    fn golden_fixture_round_trips_through_the_dual_shape() {
        // deny_unknown_fields is the conformance authority: the frozen JSON
        // must parse back into the exact card, so the published document can
        // never carry a field the crate does not model.
        let parsed: AgentRegistration = serde_json::from_str(GOLDEN).unwrap();
        assert_eq!(parsed, covenant_foundation_card());
    }

    #[test]
    fn foundation_identity_values_are_preserved() {
        let card = covenant_foundation_card();
        // Solana identity: the pubkey rides the did:pkh subject in CAIP-2
        // genesis form, never a network-name form like solana:101.
        let did = card
            .services
            .iter()
            .find(|s| s.name == "DID")
            .expect("DID service")
            .endpoint
            .clone();
        assert_eq!(
            did,
            format!("did:pkh:{SOLANA_MAINNET_CAIP2}:{FOUNDATION_PUBKEY_BASE58}")
        );
        assert_eq!(
            card.registrations[0].agent_registry,
            format!("{SOLANA_MAINNET_CAIP2}:{FOUNDATION_SOLANA_REGISTRY}")
        );
        assert_eq!(card.registrations[0].agent_id, 0);
        // Base ERC-8004 registration: the minted agentId and registry.
        assert_eq!(card.registrations[1].agent_id, FOUNDATION_BASE_AGENT_ID);
        assert_eq!(
            card.registrations[1].agent_registry,
            FOUNDATION_BASE_REGISTRY
        );
        assert!(!GOLDEN.contains("solana:101"));
    }

    #[test]
    fn card_signs_and_verifies() {
        // The operator signing step must work on this exact content: the
        // Base agentId is JCS-safe and the body canonicalizes.
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut card = covenant_foundation_card();
        card.sign(&key).unwrap();
        assert!(card.verify(&key.verifying_key()).is_ok());
    }
}
