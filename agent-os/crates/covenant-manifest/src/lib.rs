//! Parser and validator for Covenant agent manifests (`agent.toml`).

#![deny(unsafe_code)]

use covenant_types::Priority;
use serde::Deserialize;
use std::path::{Component, Path};
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub agent: Agent,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub sandbox: Sandbox,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Sandbox {
    pub required: bool,
    pub backend: SandboxBackend,
    pub filesystem: FilesystemPolicy,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            required: false,
            backend: SandboxBackend::TrustedLocal,
            filesystem: FilesystemPolicy::ReadOnlyPackage,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    #[default]
    TrustedLocal,
    LinuxGvisor,
}

impl SandboxBackend {
    pub fn is_sandbox_grade(self) -> bool {
        matches!(self, Self::LinuxGvisor)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemPolicy {
    #[default]
    ReadOnlyPackage,
    Ephemeral,
    Host,
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
        // `agent.id` flows into a synthesised `AgentId.display` of shape
        // `<id>@agent` for budget keying (covenantd::agent_id_for_card).
        // The display is round-tripped through `AgentId`'s serde, which
        // calls `validate_agent_id_display`'s `[A-Za-z0-9_.-]+` filter on
        // each side — so an id with characters outside that set boots
        // fine but breaks JSONL replay on next reopen. Catch it here.
        if !self
            .agent
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        {
            return Err(ManifestError::Validation(format!(
                "agent.id {:?} must be ASCII [A-Za-z0-9_.-]+",
                self.agent.id
            )));
        }
        let entry_path = Path::new(&self.agent.entry);
        if entry_path.is_absolute()
            || entry_path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::Prefix(_) | Component::RootDir
                )
            })
        {
            return Err(ManifestError::Validation(
                "agent.entry must be a relative path inside the agent package".into(),
            ));
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
        if self.sandbox.required && !self.sandbox.backend.is_sandbox_grade() {
            return Err(ManifestError::Validation(
                "sandbox.required=true requires a sandbox-grade backend".into(),
            ));
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

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "read-only-package"

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
        assert!(m.sandbox.required);
        assert_eq!(m.sandbox.backend, SandboxBackend::LinuxGvisor);
        assert_eq!(m.sandbox.filesystem, FilesystemPolicy::ReadOnlyPackage);
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
        assert!(!m.sandbox.required);
        assert_eq!(m.sandbox.backend, SandboxBackend::TrustedLocal);
        assert_eq!(m.sandbox.filesystem, FilesystemPolicy::ReadOnlyPackage);
        assert_eq!(m.settlement.budget_credits_per_hour, 0);
        assert_eq!(m.settlement.priority, Priority::Normal);
    }

    #[test]
    fn rejects_required_trusted_local_sandbox() {
        let bad = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"

[sandbox]
required = true
backend = "trusted-local"
"#;
        match Manifest::parse(bad) {
            Err(ManifestError::Validation(msg)) => {
                assert!(msg.contains("sandbox.required=true"), "{msg}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_id_with_disallowed_chars() {
        // `agent.id` flows into `<id>@agent` for budget keying; chars
        // outside `[A-Za-z0-9_.-]` would boot fine but trip
        // `validate_agent_id_display` on JSONL replay. Reject early.
        for bad in ["foo bar", "foo@host", "foo:bar", "fooé", "foo/bar"] {
            let toml = format!(
                r#"
[agent]
id = "{bad}"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#
            );
            match Manifest::parse(&toml) {
                Err(ManifestError::Validation(msg)) => {
                    assert!(msg.contains("agent.id"), "{bad}: {msg}");
                }
                other => panic!("expected validation error for {bad:?}, got {other:?}"),
            }
        }
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
    fn rejects_entry_outside_package() {
        for entry in ["/bin/sh", "../agent.sh", "subdir/../../agent.sh"] {
            let bad = format!(
                r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "{entry}"
"#
            );
            match Manifest::parse(&bad) {
                Err(ManifestError::Validation(msg)) => {
                    assert!(msg.contains("agent.entry"), "{msg}");
                }
                other => panic!("expected validation error for {entry:?}, got {other:?}"),
            }
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
