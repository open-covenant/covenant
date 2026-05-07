//! Parser and validator for Covenant agent manifests (`agent.toml`).

#![deny(unsafe_code)]

use covenant_types::Priority;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub agent: Agent,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub settlement: Settlement,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub runtime: Runtime,
    pub entry: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Python3,
    Node,
    #[serde(rename = "rust-bin")]
    RustBin,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub required: Vec<String>,
    pub optional: Vec<String>,
}

/// Defaults match the spec §5 example.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Resources {
    pub cpu_ms_per_task: u64,
    pub memory_mb: u64,
    pub disk_mb: u64,
    pub network: NetworkPolicy,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpu_ms_per_task: 30_000,
            memory_mb: 512,
            disk_mb: 100,
            network: NetworkPolicy::OutboundHttpsOnly,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPolicy {
    Off,
    #[default]
    OutboundHttpsOnly,
    Full,
}

/// Phase 0 tolerates `0`; enforced from Phase 1.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settlement {
    pub budget_credits_per_hour: u64,
    pub priority: Priority,
}

/// Reserved capability namespaces (spec §5).
pub const RESERVED_NAMESPACES: &[&str] = &["intent.", "memory.", "identity.", "tool.", "agent."];

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("toml parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("validation: {0}")]
    Validation(String),
}

impl Manifest {
    pub fn parse(s: &str) -> Result<Self, ManifestError> {
        let m: Self = toml::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    pub fn from_path(p: &Path) -> Result<Self, ManifestError> {
        let s = std::fs::read_to_string(p)?;
        Self::parse(&s)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        for (field, value) in [
            ("agent.id", &self.agent.id),
            ("agent.name", &self.agent.name),
            ("agent.version", &self.agent.version),
            ("agent.entry", &self.agent.entry),
        ] {
            if value.is_empty() {
                return Err(ManifestError::Validation(format!(
                    "{field} must not be empty"
                )));
            }
        }
        for cap in self
            .capabilities
            .required
            .iter()
            .chain(self.capabilities.optional.iter())
        {
            if !RESERVED_NAMESPACES.iter().any(|ns| cap.starts_with(ns)) {
                return Err(ManifestError::Validation(format!(
                    "capability '{cap}' must use one of: {RESERVED_NAMESPACES:?}"
                )));
            }
        }
        Ok(())
    }
}

impl FromStr for Manifest {
    type Err = ManifestError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[agent]
id = "research"
name = "Research Agent"
version = "0.1.0"
runtime = "python3"
entry = "main.py"

[capabilities]
required = ["intent.subscribe", "memory.write", "tool.web_search"]
optional = ["tool.gpu_inference"]

[resources]
cpu_ms_per_task = 30000
memory_mb = 512
disk_mb = 100
network = "outbound-https-only"

[settlement]
budget_credits_per_hour = 10
priority = "normal"
"#;

    const MINIMAL: &str = r#"
[agent]
id = "tiny"
name = "Tiny"
version = "0.0.1"
runtime = "rust-bin"
entry = "./tiny"
"#;

    #[test]
    fn parses_full_spec_example() {
        let m = Manifest::parse(FULL).unwrap();
        assert_eq!(m.agent.id, "research");
        assert_eq!(m.agent.runtime, Runtime::Python3);
        assert_eq!(m.capabilities.required.len(), 3);
        assert_eq!(m.capabilities.optional, vec!["tool.gpu_inference"]);
        assert_eq!(m.resources.cpu_ms_per_task, 30_000);
        assert_eq!(m.resources.network, NetworkPolicy::OutboundHttpsOnly);
        assert_eq!(m.settlement.budget_credits_per_hour, 10);
        assert_eq!(m.settlement.priority, Priority::Normal);
    }

    #[test]
    fn parses_minimal_with_defaults() {
        let m = Manifest::parse(MINIMAL).unwrap();
        assert_eq!(m.agent.runtime, Runtime::RustBin);
        assert!(m.capabilities.required.is_empty());
        assert!(m.capabilities.optional.is_empty());
        assert_eq!(m.resources.cpu_ms_per_task, 30_000);
        assert_eq!(m.resources.memory_mb, 512);
        assert_eq!(m.resources.disk_mb, 100);
        assert_eq!(m.resources.network, NetworkPolicy::OutboundHttpsOnly);
        assert_eq!(m.settlement.budget_credits_per_hour, 0);
        assert_eq!(m.settlement.priority, Priority::Normal);
    }

    #[test]
    fn rejects_empty_id() {
        let bad = r#"
[agent]
id = ""
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;
        match Manifest::parse(bad) {
            Err(ManifestError::Validation(msg)) => assert!(msg.contains("agent.id")),
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_capability_namespace() {
        let bad = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "python3"
entry = "x.py"

[capabilities]
required = ["unknown.thing"]
"#;
        match Manifest::parse(bad) {
            Err(ManifestError::Validation(msg)) => assert!(msg.contains("unknown.thing")),
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_runtime_at_parse_time() {
        let bad = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "fortran"
entry = "x.f90"
"#;
        assert!(matches!(Manifest::parse(bad), Err(ManifestError::Parse(_))));
    }

    #[test]
    fn parses_via_fromstr_trait() {
        let m: Manifest = MINIMAL.parse().unwrap();
        assert_eq!(m.agent.id, "tiny");
    }

    #[test]
    fn from_path_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, FULL).unwrap();
        let m = Manifest::from_path(&path).unwrap();
        assert_eq!(m.agent.id, "research");
    }
}
