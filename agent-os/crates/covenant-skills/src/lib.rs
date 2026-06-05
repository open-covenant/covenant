//! Skill manifest model for Covenant.
//!
//! A [`SkillManifest`] is the typed view of one Anthropic-format Agent Skill
//! (`SKILL.md` + `references/**`) installed via `npx skills add`. Covenant
//! consumes manifests to gate skill use against signed capabilities, anchor
//! the running skill's content into the audit chain, and refuse to sign any
//! on-chain transaction whose `chain.tx.{program}.{ix}` is not pre-authorized.
//!
//! Parsing `SKILL.md` and computing the content [`digest`](SkillManifest)
//! ships in a sibling slice; this crate is the data shape + serde contract
//! the rest of the workspace targets.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

/// One installed Solana Agent Skill, as Covenant understands it.
///
/// The fields below are the union of what `SKILL.md` frontmatter declares,
/// what Covenant computes (`digest`, `source.commit`), and what the
/// skill author declares about its on-chain surface (`declared_programs`,
/// `sends_tx`). Capability grants are authored separately and live in
/// [`SkillManifest::declared_capabilities`] as `chain.tx.{program}.{ix}` /
/// `skill.use.{name}` strings — the actual grant + scope are checked
/// against `covenant-permissions` at use-time, never trusted from the
/// manifest alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub digest: String,
    pub source: SkillSource,
    #[serde(default)]
    pub declared_capabilities: Vec<String>,
    #[serde(default)]
    pub declared_programs: Vec<String>,
    #[serde(default)]
    pub sends_tx: bool,
}

/// Pinned origin coordinates for a skill — matches the Solana Foundation
/// `communitySkills.ts` rule that listing URLs are immutable
/// `https://github.com/<owner>/<repo>/tree/<tag>/<path>` references. `commit`
/// is the resolved SHA for `<tag>` at install time; it is what the audit
/// chain anchors against so a re-tagged release cannot be silently swapped
/// under a running deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSource {
    pub url: String,
    pub tag: String,
    pub commit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SkillManifest {
        SkillManifest {
            name: "covenant".to_string(),
            version: "0.1.0".to_string(),
            description: "verifiable agent execution on Solana".to_string(),
            digest: "sha256:0".repeat(8),
            source: SkillSource {
                url: "https://github.com/open-covenant/covenant-skill/tree/v0.1.0/skill"
                    .to_string(),
                tag: "v0.1.0".to_string(),
                commit: "0".repeat(40),
            },
            declared_capabilities: vec![
                "skill.use.covenant".to_string(),
                "chain.tx.system.transfer".to_string(),
            ],
            declared_programs: vec!["11111111111111111111111111111111".to_string()],
            sends_tx: true,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let m = sample();
        let json = serde_json::to_string(&m).expect("serialize");
        let back: SkillManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, m);
    }

    #[test]
    fn defaults_apply_when_optional_fields_are_absent() {
        let json = r#"{
            "name": "minimal",
            "version": "0.0.1",
            "description": "no on-chain surface",
            "digest": "sha256:abc",
            "source": {
                "url": "https://example.invalid/tree/v0/skill",
                "tag": "v0",
                "commit": "0000000000000000000000000000000000000000"
            }
        }"#;
        let m: SkillManifest = serde_json::from_str(json).expect("deserialize");
        assert!(m.declared_capabilities.is_empty());
        assert!(m.declared_programs.is_empty());
        assert!(!m.sends_tx);
    }

    #[test]
    fn rejects_missing_required_fields() {
        let json = r#"{ "name": "incomplete" }"#;
        let err = serde_json::from_str::<SkillManifest>(json).unwrap_err();
        assert!(
            err.is_data(),
            "missing required fields must surface as data error"
        );
    }

    #[test]
    fn declared_capabilities_preserve_author_order() {
        let mut m = sample();
        m.declared_capabilities = vec![
            "z.last".to_string(),
            "m.middle".to_string(),
            "a.first".to_string(),
        ];
        let json = serde_json::to_string(&m).expect("serialize");
        let back: SkillManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.declared_capabilities,
            vec!["z.last", "m.middle", "a.first"],
            "capability order is authored intent, not sorted",
        );
    }
}
