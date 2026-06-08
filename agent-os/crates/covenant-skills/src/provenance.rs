//! Reference implementation (behind the `skill-provenance` feature) for an
//! optional provenance block in `SKILL.md` frontmatter.
//!
//! A skill author may declare three optional, vendor-neutral keys:
//!
//! ```yaml
//! capabilities:                       # signed authority the skill runs under
//!   - skill.use.covenant
//!   - chain.tx.11111111111111111111111111111111.transfer
//! onchain:                            # the on-chain surface it declares it touches
//!   programs: ["11111111111111111111111111111111"]
//!   sends_tx: true
//!   clusters: [devnet]
//! provenance:                         # how a verifier pins + checks it
//!   source: https://github.com/open-covenant/covenant-skill/tree/v0.2.0/skill
//!   digest_algorithm: covenant-skill-digest-v1
//! ```
//!
//! [`SkillProvenance::validate`] checks the declaration is internally
//! consistent; [`SkillProvenance::audit_surface`] is the catalog-CI check that
//! flags on-chain surface present in the skill's content but **not** declared in
//! frontmatter. None of this is wired into install or capability enforcement —
//! Covenant gates on signed capabilities at use-time regardless. This module is
//! the proposed convention plus a checker, nothing more.

use crate::parser::{split_frontmatter, SkillParseError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The optional provenance declaration a skill author may add to `SKILL.md`
/// frontmatter. Every field is optional; a skill that declares none of the
/// three keys parses to `None` (see [`parse_skill_provenance`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SkillProvenance {
    /// Capability action strings the skill expects to run under, e.g.
    /// `skill.use.<name>` and `chain.tx.<program>.<ix>`. Declaration only — the
    /// signed grant and its scope are still checked at use-time, never trusted
    /// from the manifest.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The on-chain surface the skill declares it touches.
    #[serde(default)]
    pub onchain: OnchainSurface,
    /// How a verifier pins and checks this skill.
    #[serde(default)]
    pub provenance: Option<ProvenanceAttestation>,
}

/// The on-chain surface a skill declares: the programs it touches, whether it
/// signs transactions, and the clusters it targets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OnchainSurface {
    #[serde(default)]
    pub programs: Vec<String>,
    #[serde(default)]
    pub sends_tx: bool,
    #[serde(default)]
    pub clusters: Vec<String>,
}

/// Verifier-facing provenance metadata: the immutable source coordinate and the
/// digest convention a consumer recomputes to pin the skill.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProvenanceAttestation {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub digest_algorithm: Option<String>,
}

/// A consistency problem found inside a declaration by
/// [`SkillProvenance::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "finding", rename_all = "snake_case")]
pub enum ProvenanceFinding {
    /// A `chain.tx.<program>.<ix>` capability names a program absent from
    /// `onchain.programs`. A skill cannot claim signing authority over a program
    /// it did not declare it touches.
    UndeclaredProgram { capability: String, program: String },
    /// `onchain.sends_tx` is true but no `chain.tx.*` capability is declared —
    /// signing a transaction needs a capability to run under.
    SendsTxWithoutCapability,
    /// A capability string is neither `skill.use.<name>` nor
    /// `chain.tx.<program>.<ix>` with non-empty segments.
    MalformedCapability { capability: String },
    /// A declared program is not a base58 Solana address (32–44 chars, the
    /// bitcoin alphabet — no `0`, `O`, `I`, `l`).
    NonBase58Program { program: String },
    /// A declared cluster is not one of devnet/localnet/testnet/mainnet-beta.
    UnknownCluster { cluster: String },
}

/// Undeclared on-chain surface found in a skill's content by
/// [`SkillProvenance::audit_surface`] — the catalog-CI signal that a skill
/// *does* more than its frontmatter declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "finding", rename_all = "snake_case")]
pub enum SurfaceFinding {
    /// The content references the transaction-proposal tool (`solana_propose_tx`)
    /// while `onchain.sends_tx` is not declared true.
    UndeclaredTxSurface,
    /// The content references `chain.tx.<program>.<ix>` for a base58 program not
    /// in `onchain.programs`.
    UndeclaredProgramSurface { program: String },
}

const KNOWN_CLUSTERS: [&str; 4] = ["devnet", "localnet", "testnet", "mainnet-beta"];
const TX_PROPOSAL_MARKER: &str = "solana_propose_tx";
const CHAIN_TX_PREFIX: &str = "chain.tx.";

/// Parse the optional provenance block from a `SKILL.md`'s frontmatter. Returns
/// `Ok(None)` when none of `capabilities` / `onchain` / `provenance` are present
/// so an existing skill is never treated as having an empty declaration.
pub fn parse_skill_provenance(content: &str) -> Result<Option<SkillProvenance>, SkillParseError> {
    let (frontmatter_yaml, _body) = split_frontmatter(content)?;
    let value: serde_yaml::Value = serde_yaml::from_str(&frontmatter_yaml)?;
    let serde_yaml::Value::Mapping(mapping) = &value else {
        return Ok(None);
    };
    let declared = mapping.iter().any(|(k, _)| {
        matches!(
            k.as_str(),
            Some("capabilities") | Some("onchain") | Some("provenance")
        )
    });
    if !declared {
        return Ok(None);
    }
    Ok(Some(serde_yaml::from_value(value)?))
}

impl SkillProvenance {
    /// Check the declaration is internally consistent. An empty result means the
    /// declaration is coherent; it does **not** mean the skill is trustworthy —
    /// signed capabilities are still enforced at use-time.
    pub fn validate(&self) -> Vec<ProvenanceFinding> {
        let mut findings = Vec::new();

        for program in &self.onchain.programs {
            if !is_base58_program(program) {
                findings.push(ProvenanceFinding::NonBase58Program {
                    program: program.clone(),
                });
            }
        }

        let declared: BTreeSet<&str> = self.onchain.programs.iter().map(String::as_str).collect();
        let mut has_chain_tx = false;
        for cap in &self.capabilities {
            match parse_capability(cap) {
                Some(Capability::SkillUse) => {}
                Some(Capability::ChainTx { program }) => {
                    has_chain_tx = true;
                    if !declared.contains(program) {
                        findings.push(ProvenanceFinding::UndeclaredProgram {
                            capability: cap.clone(),
                            program: program.to_string(),
                        });
                    }
                }
                None => findings.push(ProvenanceFinding::MalformedCapability {
                    capability: cap.clone(),
                }),
            }
        }

        if self.onchain.sends_tx && !has_chain_tx {
            findings.push(ProvenanceFinding::SendsTxWithoutCapability);
        }

        for cluster in &self.onchain.clusters {
            if !KNOWN_CLUSTERS.contains(&cluster.as_str()) {
                findings.push(ProvenanceFinding::UnknownCluster {
                    cluster: cluster.clone(),
                });
            }
        }

        findings
    }

    /// Flag on-chain surface present in `content` (the skill body plus any
    /// reference text, concatenated by the caller) that the declaration does not
    /// cover. This is the PoC a catalog's CI would run to refuse a skill whose
    /// declared surface understates what it actually does.
    pub fn audit_surface(&self, content: &str) -> Vec<SurfaceFinding> {
        let mut findings = Vec::new();
        if content.contains(TX_PROPOSAL_MARKER) && !self.onchain.sends_tx {
            findings.push(SurfaceFinding::UndeclaredTxSurface);
        }
        let declared: BTreeSet<&str> = self.onchain.programs.iter().map(String::as_str).collect();
        let mut seen = BTreeSet::new();
        for program in scan_chain_tx_programs(content) {
            if !declared.contains(program) && seen.insert(program) {
                findings.push(SurfaceFinding::UndeclaredProgramSurface {
                    program: program.to_string(),
                });
            }
        }
        findings
    }
}

enum Capability<'a> {
    SkillUse,
    ChainTx { program: &'a str },
}

fn parse_capability(action: &str) -> Option<Capability<'_>> {
    if let Some(name) = action.strip_prefix("skill.use.") {
        return (!name.is_empty()).then_some(Capability::SkillUse);
    }
    if let Some(rest) = action.strip_prefix(CHAIN_TX_PREFIX) {
        let (program, instruction) = rest.split_once('.')?;
        if program.is_empty() || instruction.is_empty() {
            return None;
        }
        return Some(Capability::ChainTx { program });
    }
    None
}

/// Every base58-looking program id referenced via `chain.tx.<program>.` in
/// free text. Placeholders like `chain.tx.<program-id>.ix` are skipped because
/// they are not 32–44 base58 characters, keeping the catalog check low-noise.
fn scan_chain_tx_programs(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(idx) = rest.find(CHAIN_TX_PREFIX) {
        let after = &rest[idx + CHAIN_TX_PREFIX.len()..];
        if let Some(dot) = after.find('.') {
            let program = &after[..dot];
            if is_base58_program(program) {
                out.push(program);
            }
        }
        rest = after;
    }
    out
}

/// Mirror of the SDK `assertSolanaAddress` / `SOLANA_ADDRESS_REGEX` rule:
/// 32–44 base58 characters (the bitcoin alphabet excludes `0`, `O`, `I`, `l`).
fn is_base58_program(value: &str) -> bool {
    let len = value.len();
    (32..=44).contains(&len)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

    fn with_provenance() -> String {
        format!(
            "---\nname: covenant\ndescription: d\ncapabilities:\n  - skill.use.covenant\n  \
             - chain.tx.{SYSTEM_PROGRAM}.transfer\nonchain:\n  programs:\n    - \"{SYSTEM_PROGRAM}\"\n  \
             sends_tx: true\n  clusters: [devnet]\nprovenance:\n  source: \
             https://github.com/open-covenant/covenant-skill/tree/v0.2.0/skill\n  \
             digest_algorithm: covenant-skill-digest-v1\n---\n\nbody\n"
        )
    }

    #[test]
    fn parse_returns_none_without_provenance_keys() {
        let plain =
            "---\nname: covenant\ndescription: d\nmetadata:\n  version: 0.2.0\n---\n\nbody\n";
        assert_eq!(parse_skill_provenance(plain).expect("parse"), None);
    }

    #[test]
    fn parse_reads_declared_blocks() {
        let p = parse_skill_provenance(&with_provenance())
            .expect("parse")
            .expect("declared");
        assert_eq!(
            p.capabilities,
            vec![
                "skill.use.covenant".to_string(),
                format!("chain.tx.{SYSTEM_PROGRAM}.transfer"),
            ]
        );
        assert_eq!(p.onchain.programs, vec![SYSTEM_PROGRAM.to_string()]);
        assert!(p.onchain.sends_tx);
        assert_eq!(p.onchain.clusters, vec!["devnet".to_string()]);
        let attestation = p.provenance.expect("provenance");
        assert_eq!(
            attestation.digest_algorithm.as_deref(),
            Some("covenant-skill-digest-v1")
        );
    }

    #[test]
    fn parse_propagates_frontmatter_error() {
        let no_delim = "name: covenant\nbody without frontmatter\n";
        assert!(matches!(
            parse_skill_provenance(no_delim),
            Err(SkillParseError::MissingOpenDelimiter)
        ));
    }

    #[test]
    fn validate_clean_declaration_is_empty() {
        let p = parse_skill_provenance(&with_provenance())
            .expect("parse")
            .expect("declared");
        assert!(
            p.validate().is_empty(),
            "a coherent declaration has no findings: {:?}",
            p.validate()
        );
    }

    #[test]
    fn validate_flags_chain_tx_program_not_in_onchain() {
        let p = SkillProvenance {
            capabilities: vec![format!("chain.tx.{SYSTEM_PROGRAM}.transfer")],
            onchain: OnchainSurface {
                programs: vec![], // declares the capability but not the program
                sends_tx: true,
                clusters: vec![],
            },
            provenance: None,
        };
        assert!(p
            .validate()
            .contains(&ProvenanceFinding::UndeclaredProgram {
                capability: format!("chain.tx.{SYSTEM_PROGRAM}.transfer"),
                program: SYSTEM_PROGRAM.to_string(),
            }));
    }

    #[test]
    fn validate_flags_sends_tx_without_capability() {
        let p = SkillProvenance {
            capabilities: vec!["skill.use.covenant".to_string()],
            onchain: OnchainSurface {
                programs: vec![],
                sends_tx: true,
                clusters: vec![],
            },
            provenance: None,
        };
        assert!(p
            .validate()
            .contains(&ProvenanceFinding::SendsTxWithoutCapability));
    }

    #[test]
    fn validate_flags_malformed_capability() {
        let p = SkillProvenance {
            capabilities: vec!["not.a.known.namespace".to_string()],
            ..Default::default()
        };
        assert!(p
            .validate()
            .contains(&ProvenanceFinding::MalformedCapability {
                capability: "not.a.known.namespace".to_string(),
            }));
    }

    #[test]
    fn validate_flags_non_base58_program() {
        let p = SkillProvenance {
            onchain: OnchainSurface {
                programs: vec!["not-a-base58-address".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(p.validate().contains(&ProvenanceFinding::NonBase58Program {
            program: "not-a-base58-address".to_string(),
        }));
    }

    #[test]
    fn validate_flags_unknown_cluster() {
        let p = SkillProvenance {
            onchain: OnchainSurface {
                clusters: vec!["mainnet".to_string()], // not "mainnet-beta"
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(p.validate().contains(&ProvenanceFinding::UnknownCluster {
            cluster: "mainnet".to_string(),
        }));
    }

    #[test]
    fn audit_surface_flags_undeclared_tx_marker() {
        let p = SkillProvenance {
            onchain: OnchainSurface {
                sends_tx: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let body = "the agent calls solana_propose_tx to move lamports";
        assert_eq!(
            p.audit_surface(body),
            vec![SurfaceFinding::UndeclaredTxSurface]
        );
    }

    #[test]
    fn audit_surface_flags_undeclared_program_in_content() {
        let p = SkillProvenance::default();
        let body = format!("see chain.tx.{SYSTEM_PROGRAM}.transfer for the memo flow");
        assert_eq!(
            p.audit_surface(&body),
            vec![SurfaceFinding::UndeclaredProgramSurface {
                program: SYSTEM_PROGRAM.to_string(),
            }]
        );
    }

    #[test]
    fn audit_surface_ignores_placeholder_program() {
        let p = SkillProvenance::default();
        // `<program-id>` is not 32–44 base58 chars, so the catalog check stays
        // quiet on documentation placeholders.
        let body = "grant chain.tx.<program-id>.<instruction> before signing";
        assert!(p.audit_surface(body).is_empty());
    }

    #[test]
    fn audit_surface_clean_when_declared() {
        let p = parse_skill_provenance(&with_provenance())
            .expect("parse")
            .expect("declared");
        let body =
            format!("the agent calls solana_propose_tx against chain.tx.{SYSTEM_PROGRAM}.transfer");
        assert!(
            p.audit_surface(&body).is_empty(),
            "declared tx + program surface must not be flagged: {:?}",
            p.audit_surface(&body)
        );
    }

    #[test]
    fn declaration_round_trips_through_yaml() {
        let p = parse_skill_provenance(&with_provenance())
            .expect("parse")
            .expect("declared");
        let yaml = serde_yaml::to_string(&p).expect("serialize");
        let back: SkillProvenance = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(back, p);
    }
}
