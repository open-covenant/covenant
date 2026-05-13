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
    /// Hermes-runtime-specific config. Ignored when `agent.runtime` is
    /// not `Hermes`. The fields are documentary today — Hermes manages
    /// its tool allowlist server-side — but they pin the contract the
    /// agent author expects, surface in operator listings, and act as
    /// the seam for future per-run enforcement once Hermes exposes one.
    #[serde(default)]
    pub hermes: HermesAgent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub runtime: Runtime,
    /// Required for subprocess-style runtimes (python3, node, rust-bin).
    /// Ignored for service-delegated runtimes (hermes) and may be omitted.
    #[serde(default)]
    pub entry: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Python3,
    Node,
    #[serde(rename = "rust-bin")]
    RustBin,
    /// Agent runs inside Hermes (https://github.com/NousResearch/hermes-agent)
    /// over its HTTP API. The daemon dispatches the intent to a configured
    /// Hermes endpoint; `entry` is ignored and may be left at its placeholder.
    Hermes,
}

impl Runtime {
    /// `true` for runtimes that need an executable on disk in the package
    /// directory (subprocess-style). `false` for runtimes that delegate to
    /// an external service (currently just Hermes).
    pub fn requires_package_entry(self) -> bool {
        match self {
            Runtime::Python3 | Runtime::Node | Runtime::RustBin => true,
            Runtime::Hermes => false,
        }
    }
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

/// `[hermes]` block in `agent.toml`. All fields default to empty/None;
/// presence is what matters. See `Manifest::hermes`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HermesAgent {
    /// Tools the agent author expects the run to invoke. Names match
    /// Hermes's tool-registry slugs (e.g. `"terminal"`, `"read_file"`,
    /// `"web"`). Documentary today — operators can spot a manifest that
    /// over-asks before granting capabilities — and an enforcement hook
    /// once Hermes accepts per-run tool allowlists.
    pub tools_allowed: Vec<String>,
    /// How the runner should handle Hermes `approval.request` events
    /// when no operator is online. `operator-prompt` is the default
    /// (block until an operator answers via the console). `auto-deny`
    /// short-circuits to a denied response. `auto-once` accepts once and
    /// stops. Reserved — runtime enforcement lands when the runner
    /// learns to post `/v1/runs/{id}/approval`.
    pub approval_policy: HermesApprovalPolicy,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HermesApprovalPolicy {
    #[default]
    OperatorPrompt,
    AutoDeny,
    AutoOnce,
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
        let mut required_fields: Vec<(&str, &String)> = vec![
            ("agent.id", &self.agent.id),
            ("agent.name", &self.agent.name),
            ("agent.version", &self.agent.version),
        ];
        // Runtimes that spawn from the package dir need a real entry path.
        // Service-delegated runtimes (Hermes) ignore it entirely.
        if self.agent.runtime.requires_package_entry() {
            required_fields.push(("agent.entry", &self.agent.entry));
        }
        for (field, value) in required_fields {
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
        if self.agent.runtime.requires_package_entry() {
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
    fn reserved_namespaces_pins_exact_entries_count_order_and_trailing_dot_invariant() {
        // RESERVED_NAMESPACES (line 183) is the public capability-namespace
        // whitelist consumed by Manifest::validate at line 263. The const
        // is an &[&str] of five entries — 'intent.', 'memory.', 'identity.',
        // 'tool.', 'agent.' — each ending with a literal '.' that is
        // load-bearing for the starts_with() prefix match.
        //
        // validate_rejects_bad_optional_capability_namespace and
        // rejects_bad_capability_namespace pin REJECTION through the
        // 'unknown.thing' fixture, but neither test pins the literal
        // whitelist contents. A refactor that dropped 'identity.' would
        // silently break manifests declaring identity.publish_key; a
        // refactor that dropped the trailing '.' from 'tool.' would
        // silently widen the match so 'toolkit.scope_creep' slips
        // through; a refactor that added an unscoped namespace (e.g.,
        // 'experimental.') would silently let manifests bind
        // experimental.foo at parse time even though the daemon has no
        // dispatch path for it.
        assert_eq!(
            RESERVED_NAMESPACES.len(),
            5,
            "RESERVED_NAMESPACES must contain exactly five entries — \
             intent., memory., identity., tool., agent.; a refactor that \
             added or removed an entry without coordinating with the \
             capability-action dispatch path would split the failure \
             across manifest-accept and dispatch-reject surfaces",
        );

        assert_eq!(
            RESERVED_NAMESPACES,
            &["intent.", "memory.", "identity.", "tool.", "agent."],
            "RESERVED_NAMESPACES must be the exact ordered slice \
             [intent., memory., identity., tool., agent.] — pinning the \
             literal contents so a refactor that dropped 'identity.' \
             under a 'fold into agent.' rationale surfaces here loud \
             rather than via every operator's identity.publish_key \
             manifest suddenly failing validate. The iteration order is \
             the public order documented for admin UI and docs \
             generators that enumerate reserved prefixes",
        );

        for entry in RESERVED_NAMESPACES {
            assert!(
                entry.ends_with('.'),
                "RESERVED_NAMESPACES entry {entry:?} must end with '.' — \
                 the trailing dot is load-bearing for the starts_with() \
                 prefix match at line 263; a refactor that emitted \
                 'tool' (no dot) would silently let 'toolkit.scope_creep' \
                 or 'agentic.bypass' slip through validation, widening \
                 the operator-facing capability surface beyond the \
                 documented namespaces with no parse-time signal",
            );
            assert!(
                !entry.is_empty() && *entry != ".",
                "RESERVED_NAMESPACES entry {entry:?} must be a non-empty \
                 namespace followed by '.' — a defensive guard against \
                 an accidental '' or '.' that would match every \
                 capability string under starts_with() and silently \
                 disable the whitelist entirely",
            );
        }
    }

    #[test]
    fn runtime_serde_pins_each_variant_slug() {
        // Runtime carries rename_all = lowercase AND an explicit
        // #[serde(rename = "rust-bin")] for RustBin. Every agent package
        // with `runtime = "rust-bin"` in its agent.toml depends on the
        // explicit rename — without it, rename_all would emit `rustbin`
        // (no hyphen) and silently fail every rust-bin manifest at
        // daemon start. Pin all three slugs and the un-renamed form so
        // a refactor that drops the rename attribute fails loud.
        #[derive(serde::Deserialize)]
        struct Holder {
            runtime: Runtime,
        }
        let cases: [(&str, Runtime); 4] = [
            ("python3", Runtime::Python3),
            ("node", Runtime::Node),
            ("rust-bin", Runtime::RustBin),
            ("hermes", Runtime::Hermes),
        ];
        for (slug, expected) in cases {
            let toml_src = format!("runtime = \"{slug}\"");
            let parsed: Holder = toml::from_str(&toml_src)
                .unwrap_or_else(|err| panic!("Runtime slug {slug:?} must deserialize, got: {err}"));
            assert_eq!(parsed.runtime, expected);
        }

        // The default rename_all form for RustBin (rustbin, no hyphen)
        // must NOT parse — that would mean the explicit rename attribute
        // had been dropped and rust-bin manifests would also stop
        // parsing on the next refactor.
        assert!(toml::from_str::<Holder>("runtime = \"rustbin\"").is_err());

        // Titlecase must fail — the lowercase contract stays a strict
        // whitelist, not a permissive fallback.
        assert!(toml::from_str::<Holder>("runtime = \"Python3\"").is_err());
    }

    #[test]
    fn sandbox_backend_is_sandbox_grade_pins_each_variant() {
        for backend in [SandboxBackend::TrustedLocal, SandboxBackend::LinuxGvisor] {
            let expected = match backend {
                SandboxBackend::TrustedLocal => false,
                SandboxBackend::LinuxGvisor => true,
            };
            assert_eq!(
                backend.is_sandbox_grade(),
                expected,
                "{backend:?} must keep its documented sandbox-grade classification; if this fires after adding a new variant, classify it explicitly here AND in is_sandbox_grade so manifests with sandbox.required=true do not silently run on a non-sandbox backend",
            );
        }
    }

    #[test]
    fn sandbox_backend_serde_pins_each_kebab_case_slug() {
        // SandboxBackend rides on every Sandbox section parsed from
        // agent.toml. linux-gvisor is the only sandbox-grade backend
        // (sandbox_backend_is_sandbox_grade_pins_each_variant). A
        // dropped rename_all = kebab-case attribute would silently
        // refuse every existing manifest with `sandbox.backend =
        // "linux-gvisor"` and the daemon would fail to start sandboxed
        // agents with an opaque manifest-parse error.
        #[derive(serde::Deserialize)]
        struct Holder {
            backend: SandboxBackend,
        }
        let cases: [(&str, SandboxBackend); 2] = [
            ("trusted-local", SandboxBackend::TrustedLocal),
            ("linux-gvisor", SandboxBackend::LinuxGvisor),
        ];
        for (slug, expected) in cases {
            let toml_src = format!("backend = \"{slug}\"");
            let parsed: Holder = toml::from_str(&toml_src)
                .unwrap_or_else(|err| panic!("backend slug {slug:?} must deserialize, got: {err}"));
            assert_eq!(parsed.backend, expected);
        }

        // Dropping rename_all would surface variant names verbatim
        // (LinuxGvisor); the kebab-case whitelist must reject that
        // form so the regression fails loud at parse time.
        assert!(toml::from_str::<Holder>("backend = \"LinuxGvisor\"").is_err());
        // snake_case must also fail — the contract is kebab-case only,
        // not "any non-titlecase form".
        assert!(toml::from_str::<Holder>("backend = \"linux_gvisor\"").is_err());
    }

    #[test]
    fn network_policy_serde_pins_each_kebab_case_slug() {
        // NetworkPolicy rides on every Resources section parsed from
        // agent.toml. outbound-https-only is the documented default
        // (Resources::default) and the slug used in the spec §5
        // example (FULL above). A dropped rename_all = kebab-case
        // attribute would silently refuse every existing manifest
        // with `network = "outbound-https-only"` and the daemon would
        // fail to load the agent with an opaque manifest-parse error.
        #[derive(serde::Deserialize)]
        struct Holder {
            network: NetworkPolicy,
        }
        let cases: [(&str, NetworkPolicy); 3] = [
            ("off", NetworkPolicy::Off),
            ("outbound-https-only", NetworkPolicy::OutboundHttpsOnly),
            ("full", NetworkPolicy::Full),
        ];
        for (slug, expected) in cases {
            let toml_src = format!("network = \"{slug}\"");
            let parsed: Holder = toml::from_str(&toml_src)
                .unwrap_or_else(|err| panic!("network slug {slug:?} must deserialize, got: {err}"));
            assert_eq!(parsed.network, expected);
        }

        // Dropping rename_all would surface the variant name verbatim
        // (OutboundHttpsOnly); the kebab-case whitelist must reject
        // that form so the regression fails loud at parse time.
        assert!(toml::from_str::<Holder>("network = \"OutboundHttpsOnly\"").is_err());
        // The kebab-case-default form (no hyphens) for multi-word
        // variants must also fail so a future rename_all drop surfaces
        // here instead of silently passing on the unhyphenated slug.
        assert!(toml::from_str::<Holder>("network = \"outboundhttpsonly\"").is_err());
        // snake_case must also fail — the contract is kebab-case only,
        // not "any non-titlecase form".
        assert!(toml::from_str::<Holder>("network = \"outbound_https_only\"").is_err());
    }

    #[test]
    fn filesystem_policy_serde_pins_each_kebab_case_slug() {
        // FilesystemPolicy rides on every Sandbox section parsed from
        // agent.toml. read-only-package is the documented default
        // (Sandbox::default) and the slug used in the spec §5 example
        // (FULL above). A dropped rename_all = kebab-case attribute
        // would silently refuse every existing manifest with
        // `filesystem = "read-only-package"` and the daemon would
        // fail to load the agent with an opaque manifest-parse error,
        // weakening the sandbox boundary at startup.
        #[derive(serde::Deserialize)]
        struct Holder {
            filesystem: FilesystemPolicy,
        }
        let cases: [(&str, FilesystemPolicy); 3] = [
            ("read-only-package", FilesystemPolicy::ReadOnlyPackage),
            ("ephemeral", FilesystemPolicy::Ephemeral),
            ("host", FilesystemPolicy::Host),
        ];
        for (slug, expected) in cases {
            let toml_src = format!("filesystem = \"{slug}\"");
            let parsed: Holder = toml::from_str(&toml_src).unwrap_or_else(|err| {
                panic!("filesystem slug {slug:?} must deserialize, got: {err}")
            });
            assert_eq!(parsed.filesystem, expected);
        }

        // Dropping rename_all would surface the variant name verbatim
        // (ReadOnlyPackage); the kebab-case whitelist must reject
        // that form so the regression fails loud at parse time.
        assert!(toml::from_str::<Holder>("filesystem = \"ReadOnlyPackage\"").is_err());
        // The kebab-case-default form (no hyphens) for multi-word
        // variants must also fail so a future rename_all drop surfaces
        // here instead of silently passing on the unhyphenated slug.
        assert!(toml::from_str::<Holder>("filesystem = \"readonlypackage\"").is_err());
        // snake_case must also fail — the contract is kebab-case only,
        // not "any non-titlecase form".
        assert!(toml::from_str::<Holder>("filesystem = \"read_only_package\"").is_err());
    }

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
    fn validate_agent_id_pins_underscore_period_and_hyphen_as_allowed_special_chars() {
        // covenant_manifest::Manifest::validate (line 171-181) gates
        // agent.id behind:
        //
        //   b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'
        //
        // The docstring at line 167-170 and the error message at line
        // 178 jointly document the whitelist as '[A-Za-z0-9_.-]+' —
        // the same regex AgentId::validate_agent_id_display applies
        // when JSONL records replay through serde.
        //
        // rejects_id_with_disallowed_chars (line 482) pins that
        // characters OUTSIDE the whitelist (space, '@', ':', non-
        // ASCII, '/') reject. The positive complement — that the
        // three documented special characters '_', '.', and '-' are
        // ACCEPTED — is exercised only incidentally via alphanumeric-
        // only fixtures elsewhere ('research', 'sandboxed', etc.) and
        // is exercised by zero direct tests.
        //
        // A refactor that tightened the whitelist to alphanumeric-
        // only ('simplify by dropping the rarely-used special chars')
        // would silently reject every operator manifest whose
        // agent.id contains a period, hyphen, or underscore;
        // rejects_id_with_disallowed_chars would still pass because
        // those special chars would now fall into the same rejection
        // branch via the same error path. Pin each special char
        // separately AND in combination so a partial tightening (e.g.,
        // 'drop period only') surfaces on the period assertion while
        // a complete tightening surfaces on all three.
        for id in [
            "research_bot",
            "agent.v1",
            "research-bot",
            "research.bot_v1-prod",
        ] {
            let toml = format!(
                r#"
[agent]
id = "{id}"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#
            );
            let m = Manifest::parse(&toml).unwrap_or_else(|err| {
                panic!(
                    "agent.id {id:?} must be accepted by validate — the \
                     whitelist documented at line 178 is [A-Za-z0-9_.-]+ \
                     and all three special chars (underscore, period, \
                     hyphen) are documented as load-bearing for operator \
                     manifests. A refactor that tightened the whitelist \
                     to alphanumeric-only under a 'simplify' rationale \
                     would silently reject this manifest; \
                     rejects_id_with_disallowed_chars (line 482) would \
                     still pass because the special chars would fall \
                     into the rejection branch via the same error path. \
                     got: {err:?}"
                )
            });
            assert_eq!(
                m.agent.id, id,
                "the validated manifest must round-trip the agent.id \
                 verbatim — a refactor that normalised the id (e.g., \
                 lowercased it or stripped special chars during parse) \
                 would silently mutate the operator-supplied value and \
                 break audit correlation that joins on agent.id",
            );
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
    fn validate_pins_empty_rejection_for_name_version_and_entry_fields() {
        // Manifest::validate (line 152) iterates over four (field-name,
        // value) tuples and returns Err(Validation) on any empty value:
        //   ("agent.id", &self.agent.id),
        //   ("agent.name", &self.agent.name),
        //   ("agent.version", &self.agent.version),
        //   ("agent.entry", &self.agent.entry),
        //
        // rejects_empty_id pins the id tuple. The name, version, and
        // entry tuples are not individually pinned at the validate call
        // site. A refactor that drops one tuple from the iteration would
        // silently let a manifest with that field empty parse through
        // Manifest::parse and persist through load_agents_from_dir,
        // breaking downstream consumers that treat the field as
        // load-bearing. Pin each remaining arm so an arm-removal
        // regression is caught at the validate call site, mirroring the
        // covenant_types::validate_agent_id_display_pins_each_rejection_arm
        // and covenant_budget::validate_resume_state_value pin shapes.
        let empty_name = r#"
[agent]
id = "x"
name = ""
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;
        match Manifest::parse(empty_name) {
            Err(ManifestError::Validation(msg)) => {
                assert!(
                    msg.contains("agent.name"),
                    "name arm: the validation error must name 'agent.name' \
                     so operators can locate the offending field; a refactor \
                     that dropped the ('agent.name', ...) tuple would either \
                     parse successfully (silent regression) or report a \
                     different field name (misleading regression). got: {msg}"
                );
                assert!(
                    !msg.contains("agent.id")
                        && !msg.contains("agent.version")
                        && !msg.contains("agent.entry"),
                    "name arm: a copy-paste regression that pointed the \
                     ('agent.name', ...) tuple at a different field value \
                     would report the wrong field name; got: {msg}"
                );
            }
            other => panic!("expected validation error for empty name, got {other:?}"),
        }

        let empty_version = r#"
[agent]
id = "x"
name = "x"
version = ""
runtime = "node"
entry = "x.js"
"#;
        match Manifest::parse(empty_version) {
            Err(ManifestError::Validation(msg)) => {
                assert!(
                    msg.contains("agent.version"),
                    "version arm: a refactor that dropped the \
                     ('agent.version', ...) tuple ('version is for docs \
                     only, not enforced') would silently let manifests \
                     with empty version persist through load_agents_from_dir, \
                     breaking downstream tooling that pattern-matches version \
                     for compatibility decisions. got: {msg}"
                );
                assert!(
                    !msg.contains("agent.id")
                        && !msg.contains("agent.name")
                        && !msg.contains("agent.entry"),
                    "version arm: must name only 'agent.version'. got: {msg}"
                );
            }
            other => panic!("expected validation error for empty version, got {other:?}"),
        }

        let empty_entry = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = ""
"#;
        match Manifest::parse(empty_entry) {
            Err(ManifestError::Validation(msg)) => {
                assert!(
                    msg.contains("agent.entry"),
                    "entry arm: a refactor that dropped the \
                     ('agent.entry', ...) tuple on the assumption that \
                     rejects_entry_outside_package covers it would silently \
                     accept empty entry; the existing test only covers \
                     absolute paths and parent-component traversal, not \
                     empty strings — the SubprocessRunner would later \
                     attempt to launch an empty entry path with ENOENT \
                     instead of failing at manifest parse time. got: {msg}"
                );
                assert!(
                    !msg.contains("agent.id")
                        && !msg.contains("agent.name")
                        && !msg.contains("agent.version"),
                    "entry arm: must name only 'agent.entry'. got: {msg}"
                );
            }
            other => panic!("expected validation error for empty entry, got {other:?}"),
        }
    }

    #[test]
    fn hermes_section_parses_and_defaults_when_omitted() {
        let with_block = r#"
[agent]
id = "hermes-x"
name = "x"
version = "0.1.0"
runtime = "hermes"

[hermes]
tools_allowed   = ["terminal", "web"]
approval_policy = "auto-deny"
"#;
        let m = Manifest::parse(with_block).unwrap();
        assert_eq!(m.hermes.tools_allowed, vec!["terminal", "web"]);
        assert_eq!(m.hermes.approval_policy, HermesApprovalPolicy::AutoDeny);

        // Missing block deserializes to defaults — and subprocess
        // manifests stay parse-clean without forcing every legacy
        // agent.toml to add an unused [hermes] block.
        let without = r#"
[agent]
id = "py"
name = "py"
version = "0.1.0"
runtime = "python3"
entry = "main.py"
"#;
        let m = Manifest::parse(without).unwrap();
        assert!(m.hermes.tools_allowed.is_empty());
        assert_eq!(
            m.hermes.approval_policy,
            HermesApprovalPolicy::OperatorPrompt
        );
    }

    #[test]
    fn hermes_approval_policy_serde_pins_each_kebab_case_slug() {
        // The kebab-case slugs are the public contract — a refactor
        // that dropped rename_all or renamed a variant would break
        // every manifest already declaring approval_policy.
        #[derive(serde::Deserialize)]
        struct Holder {
            approval_policy: HermesApprovalPolicy,
        }
        let cases: [(&str, HermesApprovalPolicy); 3] = [
            ("operator-prompt", HermesApprovalPolicy::OperatorPrompt),
            ("auto-deny", HermesApprovalPolicy::AutoDeny),
            ("auto-once", HermesApprovalPolicy::AutoOnce),
        ];
        for (slug, expected) in cases {
            let toml_src = format!("approval_policy = \"{slug}\"");
            let parsed: Holder = toml::from_str(&toml_src).expect("kebab slug must parse");
            assert_eq!(parsed.approval_policy, expected);
        }
        assert!(toml::from_str::<Holder>("approval_policy = \"OperatorPrompt\"").is_err());
    }

    #[test]
    fn hermes_section_partial_block_independently_defaults_each_missing_field() {
        // covenant_manifest::HermesAgent (line 155-171) has
        // #[serde(default)] at the struct level so each missing
        // field falls back independently to Default::default():
        // tools_allowed -> Vec::new(), approval_policy ->
        // HermesApprovalPolicy::OperatorPrompt (via the #[default]
        // attribute on the OperatorPrompt variant at line 176).
        //
        // hermes_section_parses_and_defaults_when_omitted (line 768)
        // only covers the two binary cases: full block with both
        // fields, or no [hermes] block at all. The intermediate
        // cases — [hermes] block present with only one of
        // tools_allowed or approval_policy declared — are not tested.
        // A refactor that dropped the struct-level #[serde(default)]
        // would still parse the missing-block case (because
        // Manifest::hermes uses HermesAgent::default() via its own
        // #[serde(default)] elsewhere) but would silently start
        // rejecting every legacy manifest with a partial [hermes]
        // block.

        let only_tools = r#"
[agent]
id = "hx"
name = "hx"
version = "0.1.0"
runtime = "hermes"

[hermes]
tools_allowed = ["terminal"]
"#;
        let m = Manifest::parse(only_tools).expect(
            "partial [hermes] block with only tools_allowed must parse \
             — pins the struct-level #[serde(default)] on HermesAgent. \
             A refactor that dropped serde(default) on the struct \
             would surface here as a missing-field parse error",
        );
        assert_eq!(m.hermes.tools_allowed, vec!["terminal"]);
        assert_eq!(
            m.hermes.approval_policy,
            HermesApprovalPolicy::OperatorPrompt,
            "missing approval_policy must default to OperatorPrompt \
             — pins the #[default] attribute on the OperatorPrompt \
             variant at line 176. A refactor that removed #[default] \
             or moved it to AutoDeny would silently flip the \
             operator-prompt default and break the documented \
             contract that an unset approval_policy blocks until an \
             operator answers",
        );

        let only_policy = r#"
[agent]
id = "hx"
name = "hx"
version = "0.1.0"
runtime = "hermes"

[hermes]
approval_policy = "auto-deny"
"#;
        let m = Manifest::parse(only_policy).expect(
            "partial [hermes] block with only approval_policy must \
             parse — pins the struct-level #[serde(default)] for the \
             tools_allowed field. A refactor that swapped Vec<String> \
             for a NonEmpty wrapper would surface here as a \
             default-rejection error",
        );
        assert!(
            m.hermes.tools_allowed.is_empty(),
            "missing tools_allowed must default to empty Vec — pins \
             the documentary-today contract that an unset \
             tools_allowed places no constraint on Hermes-side tool \
             use",
        );
        assert_eq!(m.hermes.approval_policy, HermesApprovalPolicy::AutoDeny);

        let empty_block = r#"
[agent]
id = "hx"
name = "hx"
version = "0.1.0"
runtime = "hermes"

[hermes]
"#;
        let m = Manifest::parse(empty_block).expect(
            "empty [hermes] block must parse — distinct from the \
             missing-block case (covered by \
             hermes_section_parses_and_defaults_when_omitted at line \
             768) because the section header is present but fields \
             are absent. Pins that the section-present + fields- \
             absent case collapses to the same defaults as the \
             section-absent case",
        );
        assert!(m.hermes.tools_allowed.is_empty());
        assert_eq!(
            m.hermes.approval_policy,
            HermesApprovalPolicy::OperatorPrompt
        );
    }

    #[test]
    fn hermes_runtime_omits_entry() {
        // Hermes-backed agents delegate to an external gateway and have
        // nothing on disk to execute. The manifest must therefore accept
        // an omitted `entry` and skip the relative-path validation. If
        // this fires, a refactor probably tightened entry-handling
        // without consulting Runtime::requires_package_entry.
        let toml_src = r#"
[agent]
id = "hermes-research"
name = "Research via Hermes"
version = "0.1.0"
runtime = "hermes"
"#;
        let m = Manifest::parse(toml_src).expect("hermes manifest must parse without entry");
        assert_eq!(m.agent.runtime, Runtime::Hermes);
        assert_eq!(m.agent.entry, "");
    }

    #[test]
    fn subprocess_runtime_still_requires_entry() {
        // The relaxed-entry path is gated on runtime; the
        // subprocess-style runtimes must still hit the validation
        // error. Pin per-variant so a wrong predicate flip surfaces
        // here instead of in production.
        for slug in ["python3", "node", "rust-bin"] {
            let toml_src = format!(
                r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "{slug}"
"#
            );
            match Manifest::parse(&toml_src) {
                Err(ManifestError::Validation(msg)) => {
                    assert!(msg.contains("agent.entry"), "{slug}: {msg}");
                }
                other => panic!("expected validation error for {slug:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn requires_package_entry_pins_each_variant() {
        // Drives both manifest validation (above) and runtime backend
        // dispatch (covenant-runtime::CompositeRunner). Adding a new
        // Runtime variant without classifying it here would make either
        // the validator or the dispatcher silently choose a default.
        for (rt, expected) in [
            (Runtime::Python3, true),
            (Runtime::Node, true),
            (Runtime::RustBin, true),
            (Runtime::Hermes, false),
        ] {
            assert_eq!(
                rt.requires_package_entry(),
                expected,
                "{rt:?} requires_package_entry classification must stay stable"
            );
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
    fn validate_rejects_bad_optional_capability_namespace() {
        // Manifest::validate iterates capability namespaces via:
        //   self.capabilities.required.iter()
        //       .chain(self.capabilities.optional.iter())
        // (line 195-199). rejects_bad_capability_namespace pins the
        // required-list arm. The chained optional-list arm is not
        // pinned. Use empty required + an unreserved optional entry so
        // the test isolates the optional iteration arm — a refactor that
        // dropped the .chain(self.capabilities.optional.iter()) clause
        // would still pass rejects_bad_capability_namespace but fail
        // this pin loud.
        let bad = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "python3"
entry = "x.py"

[capabilities]
required = []
optional = ["unknown.thing"]
"#;
        match Manifest::parse(bad) {
            Err(ManifestError::Validation(msg)) => {
                assert!(
                    msg.contains("unknown.thing"),
                    "optional-list arm: a refactor that dropped the \
                     .chain(self.capabilities.optional.iter()) clause \
                     would silently let manifests bind an optional \
                     capability with an unreserved namespace; the daemon's \
                     capability dispatch would reject the unknown action \
                     at runtime instead of at manifest parse time, \
                     splitting the audit trail into 'manifest accepted \
                     but capability never resolves'. got: {msg}"
                );
            }
            other => panic!(
                "expected validation error for optional capability with unreserved namespace, got {other:?}"
            ),
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
    fn capabilities_section_serde_pins_two_default_bearing_fields() {
        // Capabilities is the [capabilities] block in agent.toml with two
        // fields — required: Vec<String> and optional: Vec<String> —
        // backed by the derived Default impl which returns (empty,
        // empty). The struct carries struct-level #[serde(default)] so
        // an omitted block parses with both Vecs empty, and the field-
        // level defaults (from #[derive(Default)]) also surface when the
        // block is present but unkeyed.
        //
        // parses_minimal_with_defaults exercises the omitted-block path
        // via two is_empty assertions but does not pin the field names,
        // the unkeyed-block path, or the partial-override contract that
        // lets one Vec be set without disturbing the other. A refactor
        // that renamed Capabilities::required to Capabilities::granted
        // (or applied #[serde(rename)] in either direction) would
        // silently let every agent.toml writing required = [...] parse
        // with that Vec taking its empty default, capability dispatch
        // would refuse every previously-granted call at intent time, and
        // operator deployments would see no parse-layer signal.
        let agent_block = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;

        // Full override — non-default values for both Vec fields so a
        // refactor that swapped the two bindings would be loud. Use
        // reserved-namespace capability strings so the validate()
        // cross-check (rejects_bad_capability_namespace) does not fire
        // here.
        let full = format!(
            "{agent_block}\n[capabilities]\nrequired = [\"intent.subscribe\", \"memory.write\"]\noptional = [\"tool.web_search\"]\n"
        );
        let parsed = Manifest::parse(&full).expect("full capabilities block must parse");
        assert_eq!(
            parsed.capabilities.required,
            vec!["intent.subscribe".to_string(), "memory.write".to_string()],
        );
        assert_eq!(
            parsed.capabilities.optional,
            vec!["tool.web_search".to_string()],
        );

        // Section omitted entirely → struct-level #[serde(default)]
        // returns Capabilities::default() with both Vecs empty.
        let omitted =
            Manifest::parse(agent_block).expect("manifest without [capabilities] must parse");
        assert!(
            omitted.capabilities.required.is_empty(),
            "Capabilities::required default must be the empty Vec — a \
             refactor that dropped the struct-level #[serde(default)] \
             would turn every existing minimal agent.toml (no \
             [capabilities] block) into a parse error at daemon start, \
             and operator deployments would fail to load any agent until \
             each manifest re-added an explicit empty block",
        );
        assert!(
            omitted.capabilities.optional.is_empty(),
            "Capabilities::optional default must be the empty Vec",
        );

        // Unkeyed [capabilities] block (section header present, no
        // required/optional keys) → field-level #[serde(default)] on
        // each Vec surfaces the empty default. A refactor that dropped
        // a field-level default would fail this path while the omitted-
        // block path above still passed, so the regression surfaces
        // here rather than at a downstream call site.
        let unkeyed = format!("{agent_block}\n[capabilities]\n");
        let parsed =
            Manifest::parse(&unkeyed).expect("manifest with unkeyed [capabilities] must parse");
        assert!(
            parsed.capabilities.required.is_empty(),
            "field-level #[serde(default)] on Capabilities::required \
             must surface the empty Vec when the [capabilities] block \
             is present but unkeyed",
        );
        assert!(parsed.capabilities.optional.is_empty());

        // Partial override — only required set, optional must fall back
        // to the empty Vec without the present field disturbing it.
        let only_required =
            format!("{agent_block}\n[capabilities]\nrequired = [\"intent.subscribe\"]\n");
        let parsed = Manifest::parse(&only_required).unwrap();
        assert_eq!(
            parsed.capabilities.required,
            vec!["intent.subscribe".to_string()],
        );
        assert!(parsed.capabilities.optional.is_empty());

        // Partial override — only optional set, required must fall back
        // to the empty Vec.
        let only_optional =
            format!("{agent_block}\n[capabilities]\noptional = [\"tool.web_search\"]\n");
        let parsed = Manifest::parse(&only_optional).unwrap();
        assert!(parsed.capabilities.required.is_empty());
        assert_eq!(
            parsed.capabilities.optional,
            vec!["tool.web_search".to_string()],
        );
    }

    #[test]
    fn settlement_section_serde_pins_two_default_bearing_fields() {
        // Settlement is the [settlement] block in agent.toml with two
        // fields — budget_credits_per_hour (u64) and priority
        // (covenant_types::Priority) — backed by the derived Default
        // impl which returns (0, Priority::Normal). The struct carries
        // #[serde(default)] so omitted fields fall back to the impl's
        // values.
        //
        // The struct's doc explicitly states "Phase 0 tolerates 0;
        // enforced from Phase 1" — locking 0 as the default for
        // budget_credits_per_hour keeps that contract; a refactor that
        // seeded a non-zero default would silently push agents into
        // BudgetExhausted at intent dispatch and operator deployments
        // running unbudgeted under Phase 0 would start hitting capacity
        // failures with no parse-time signal.
        let agent_block = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;

        // Full override — non-default values so a refactor that swapped
        // the two fields' bindings would be loud.
        let full = format!(
            "{agent_block}\n[settlement]\nbudget_credits_per_hour = 4242\npriority = \"high\"\n"
        );
        let parsed = Manifest::parse(&full).expect("full settlement block must parse");
        assert_eq!(parsed.settlement.budget_credits_per_hour, 4242);
        assert_eq!(parsed.settlement.priority, Priority::High);

        // Section omitted entirely → Default impl values surface.
        let omitted =
            Manifest::parse(agent_block).expect("manifest without [settlement] must parse");
        assert_eq!(
            omitted.settlement.budget_credits_per_hour, 0,
            "Settlement::budget_credits_per_hour default must be 0 — the \
             doc says 'Phase 0 tolerates 0; enforced from Phase 1', so a \
             refactor that seeded a non-zero default would silently push \
             every Phase-0 deployment into BudgetExhausted at intent \
             dispatch with no parse-time signal"
        );
        assert_eq!(
            omitted.settlement.priority,
            Priority::Normal,
            "Settlement::priority default must be Normal — a refactor that \
             swapped the default would silently re-rank every untouched \
             agent in the router's priority ordering"
        );

        // Partial overrides — each field's omission must fall through to
        // the default WITHOUT disturbing the other one.
        let only_budget = format!("{agent_block}\n[settlement]\nbudget_credits_per_hour = 999\n");
        let parsed = Manifest::parse(&only_budget).unwrap();
        assert_eq!(parsed.settlement.budget_credits_per_hour, 999);
        assert_eq!(parsed.settlement.priority, Priority::Normal);

        let only_priority = format!("{agent_block}\n[settlement]\npriority = \"low\"\n");
        let parsed = Manifest::parse(&only_priority).unwrap();
        assert_eq!(parsed.settlement.budget_credits_per_hour, 0);
        assert_eq!(parsed.settlement.priority, Priority::Low);
    }

    #[test]
    fn sandbox_section_serde_pins_three_default_bearing_fields() {
        // Sandbox is the [sandbox] block in agent.toml with three
        // fields — required (bool), backend (SandboxBackend),
        // filesystem (FilesystemPolicy) — backed by a custom Default
        // impl that returns (false, TrustedLocal, ReadOnlyPackage). The
        // struct carries #[serde(default)] so omitted fields fall back
        // to the impl's values.
        //
        // A refactor that changed Sandbox::default().required from
        // false to true would silently require every agent.toml without
        // an explicit [sandbox] block to satisfy the sandbox-grade-
        // backend cross-check — but since the default backend is
        // TrustedLocal, every untouched deployment would fail validation
        // at manifest load with no migration path.
        let agent_block = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;

        // Full override — sandbox.required=true requires a sandbox-grade
        // backend; pin the matched pair so the validation cross-check
        // does not fire here.
        let full = format!(
            "{agent_block}\n[sandbox]\nrequired = true\nbackend = \"linux-gvisor\"\nfilesystem = \"ephemeral\"\n"
        );
        let parsed = Manifest::parse(&full).expect("full sandbox block must parse");
        assert!(parsed.sandbox.required);
        assert_eq!(parsed.sandbox.backend, SandboxBackend::LinuxGvisor);
        assert_eq!(parsed.sandbox.filesystem, FilesystemPolicy::Ephemeral);

        // Section omitted entirely → Default impl values surface.
        let omitted = Manifest::parse(agent_block).expect("manifest without [sandbox] must parse");
        assert!(
            !omitted.sandbox.required,
            "Sandbox::required default must be false — a refactor to true \
             would silently require every untouched agent.toml to satisfy \
             the sandbox-grade backend check, but since the default \
             backend is TrustedLocal the cross-check fires and every \
             deployment breaks at manifest load"
        );
        assert_eq!(
            omitted.sandbox.backend,
            SandboxBackend::TrustedLocal,
            "Sandbox::backend default must be TrustedLocal — a refactor \
             to LinuxGvisor would silently route every untouched agent.\
             toml through the gVisor runner, breaking operator \
             deployments without a gVisor host"
        );
        assert_eq!(
            omitted.sandbox.filesystem,
            FilesystemPolicy::ReadOnlyPackage,
            "Sandbox::filesystem default must be ReadOnlyPackage — a \
             refactor to Host would silently widen every untouched \
             agent's filesystem access to the host"
        );

        // Partial overrides — each field's omission must fall through to
        // the default WITHOUT disturbing the others. Use safe non-default
        // combinations that still satisfy the sandbox-grade cross-check
        // (required=true with default TrustedLocal backend would fail
        // validation, so we either keep required=false or pair true with
        // linux-gvisor).
        let only_filesystem = format!("{agent_block}\n[sandbox]\nfilesystem = \"host\"\n");
        let parsed = Manifest::parse(&only_filesystem).unwrap();
        assert!(!parsed.sandbox.required);
        assert_eq!(parsed.sandbox.backend, SandboxBackend::TrustedLocal);
        assert_eq!(parsed.sandbox.filesystem, FilesystemPolicy::Host);

        let only_backend = format!("{agent_block}\n[sandbox]\nbackend = \"linux-gvisor\"\n");
        let parsed = Manifest::parse(&only_backend).unwrap();
        assert!(!parsed.sandbox.required);
        assert_eq!(parsed.sandbox.backend, SandboxBackend::LinuxGvisor);
        assert_eq!(parsed.sandbox.filesystem, FilesystemPolicy::ReadOnlyPackage);

        // required=true requires backend=linux-gvisor per the validation
        // cross-check; pair them and assert filesystem retains its
        // default so the contract is locked even when the cross-check
        // forces backend to be non-default.
        let required_with_gvisor =
            format!("{agent_block}\n[sandbox]\nrequired = true\nbackend = \"linux-gvisor\"\n");
        let parsed = Manifest::parse(&required_with_gvisor).unwrap();
        assert!(parsed.sandbox.required);
        assert_eq!(parsed.sandbox.backend, SandboxBackend::LinuxGvisor);
        assert_eq!(parsed.sandbox.filesystem, FilesystemPolicy::ReadOnlyPackage);
    }

    #[test]
    fn resources_section_serde_pins_four_default_bearing_fields() {
        // Resources is the [resources] block in agent.toml with four
        // fields — cpu_ms_per_task, memory_mb, disk_mb, network — each
        // backed by a documented Default impl that returns the spec
        // values (30_000 / 512 / 100 / OutboundHttpsOnly). The struct
        // itself carries #[serde(default)] so omitted fields fall back
        // to the impl's values.
        //
        // The existing parses_minimal_with_defaults test exercises the
        // full-default path but does not pin each individual default
        // value nor each field's name. A refactor that changed
        // Resources::default()'s cpu_ms_per_task from 30_000 to 0 would
        // silently zero-cap every agent's per-task CPU budget and every
        // intent dispatch would be capped at 0ms; a refactor that
        // renamed cpu_ms_per_task to cpu_budget_ms would silently drop
        // every operator's tuned CPU cap.
        let agent_block = r#"
[agent]
id = "x"
name = "x"
version = "0.0.1"
runtime = "node"
entry = "x.js"
"#;

        // Full override — every field set to a non-default value so a
        // refactor that swapped two fields' bindings would be loud.
        let full = format!(
            "{agent_block}\n[resources]\ncpu_ms_per_task = 1234\nmemory_mb = 256\ndisk_mb = 50\nnetwork = \"off\"\n"
        );
        let parsed = Manifest::parse(&full).expect("full resources block must parse");
        assert_eq!(parsed.resources.cpu_ms_per_task, 1234);
        assert_eq!(parsed.resources.memory_mb, 256);
        assert_eq!(parsed.resources.disk_mb, 50);
        assert_eq!(parsed.resources.network, NetworkPolicy::Off);

        // Section omitted entirely → Default impl values surface.
        let omitted =
            Manifest::parse(agent_block).expect("manifest without [resources] must parse");
        assert_eq!(
            omitted.resources.cpu_ms_per_task, 30_000,
            "Resources::cpu_ms_per_task default must be 30_000 — a \
             refactor to 0 would silently zero-cap every agent's CPU \
             budget per task; a refactor to a larger value would silently \
             widen the cap past the spec floor"
        );
        assert_eq!(
            omitted.resources.memory_mb, 512,
            "Resources::memory_mb default must be 512"
        );
        assert_eq!(
            omitted.resources.disk_mb, 100,
            "Resources::disk_mb default must be 100"
        );
        assert_eq!(
            omitted.resources.network,
            NetworkPolicy::OutboundHttpsOnly,
            "Resources::network default must be OutboundHttpsOnly — a \
             refactor to Off would silently sever every agent's outbound \
             call; a refactor to Full would silently open the network on \
             every agent that did not opt out"
        );

        // Partial overrides — each field's omission must fall through to
        // the default WITHOUT disturbing the other three.
        let only_cpu = format!("{agent_block}\n[resources]\ncpu_ms_per_task = 7777\n");
        let parsed = Manifest::parse(&only_cpu).unwrap();
        assert_eq!(parsed.resources.cpu_ms_per_task, 7777);
        assert_eq!(parsed.resources.memory_mb, 512);
        assert_eq!(parsed.resources.disk_mb, 100);
        assert_eq!(parsed.resources.network, NetworkPolicy::OutboundHttpsOnly);

        let only_memory = format!("{agent_block}\n[resources]\nmemory_mb = 2048\n");
        let parsed = Manifest::parse(&only_memory).unwrap();
        assert_eq!(parsed.resources.cpu_ms_per_task, 30_000);
        assert_eq!(parsed.resources.memory_mb, 2048);
        assert_eq!(parsed.resources.disk_mb, 100);
        assert_eq!(parsed.resources.network, NetworkPolicy::OutboundHttpsOnly);

        let only_disk = format!("{agent_block}\n[resources]\ndisk_mb = 500\n");
        let parsed = Manifest::parse(&only_disk).unwrap();
        assert_eq!(parsed.resources.cpu_ms_per_task, 30_000);
        assert_eq!(parsed.resources.memory_mb, 512);
        assert_eq!(parsed.resources.disk_mb, 500);
        assert_eq!(parsed.resources.network, NetworkPolicy::OutboundHttpsOnly);

        let only_network = format!("{agent_block}\n[resources]\nnetwork = \"full\"\n");
        let parsed = Manifest::parse(&only_network).unwrap();
        assert_eq!(parsed.resources.cpu_ms_per_task, 30_000);
        assert_eq!(parsed.resources.memory_mb, 512);
        assert_eq!(parsed.resources.disk_mb, 100);
        assert_eq!(parsed.resources.network, NetworkPolicy::Full);
    }

    #[test]
    fn manifest_serde_pins_required_agent_section_and_default_others() {
        // Manifest is the top-level agent.toml schema with five
        // sections: agent is strictly required (no serde default), and
        // capabilities / resources / sandbox / settlement each carry
        // #[serde(default)] so a minimal manifest with only [agent]
        // still parses. The existing parses_minimal_with_defaults test
        // exercises the happy path but does not pin the strictly-
        // required-agent-section contract.
        //
        // A refactor that gave Manifest::agent a #[serde(default)] would
        // silently let an empty agent.toml parse with agent.id='',
        // agent.entry='', agent.runtime defaulted to the first variant;
        // the router would key its keyword tables on the empty id,
        // SubprocessRunner would attempt Command::new("") at intent
        // dispatch, and operator deployments would lose every previously
        // routed intent path without a parse error.
        let full = Manifest::parse(FULL).expect("full manifest must parse");
        assert_eq!(full.agent.id, "research");
        assert!(!full.capabilities.required.is_empty());
        // Resources fully populated from FULL — pin one field to
        // confirm the section actually deserialised rather than landing
        // on its default.
        assert_eq!(full.resources.cpu_ms_per_task, 30_000);
        assert!(full.sandbox.required);
        assert_eq!(full.settlement.budget_credits_per_hour, 10);

        let minimal = Manifest::parse(MINIMAL).expect("minimal manifest must parse");
        // Each default-bearing section must surface its Default impl
        // values verbatim when omitted from the TOML. Pin one field per
        // section so a refactor that swapped a Default::default() return
        // value (e.g., changed Resources::cpu_ms_per_task default from
        // 30_000 to 0) is loud, and a refactor that dropped
        // #[serde(default)] from any section is also loud (the parse
        // would fail before this assertion fires).
        assert!(minimal.capabilities.required.is_empty());
        assert!(minimal.capabilities.optional.is_empty());
        assert_eq!(minimal.resources.cpu_ms_per_task, 30_000);
        assert_eq!(minimal.resources.memory_mb, 512);
        assert_eq!(minimal.resources.disk_mb, 100);
        assert_eq!(minimal.resources.network, NetworkPolicy::OutboundHttpsOnly);
        assert!(!minimal.sandbox.required);
        assert_eq!(minimal.sandbox.backend, SandboxBackend::TrustedLocal);
        assert_eq!(
            minimal.sandbox.filesystem,
            FilesystemPolicy::ReadOnlyPackage
        );
        assert_eq!(minimal.settlement.budget_credits_per_hour, 0);
        assert_eq!(minimal.settlement.priority, Priority::Normal);

        // Manifest must reject a TOML payload with NO [agent] section —
        // every other section's default does not extend to agent.
        let no_agent = r#"
[capabilities]
required = ["intent.subscribe"]

[resources]
cpu_ms_per_task = 1000
"#;
        match Manifest::parse(no_agent) {
            Err(ManifestError::Parse(_)) => {}
            other => panic!(
                "manifest with no [agent] section must fail with \
                 ManifestError::Parse — a refactor that gave Manifest::\
                 agent a #[serde(default)] would silently parse an empty \
                 manifest, the router would key on an empty agent.id, \
                 and SubprocessRunner would Command::new(\"\") at intent \
                 dispatch; got {other:?}"
            ),
        }
    }

    #[test]
    fn agent_section_serde_pins_five_required_fields() {
        // The [agent] block in every agent.toml carries five fields
        // declared without serde defaults: id, name, version, runtime,
        // entry. toml::from_str must reject any block that omits one;
        // otherwise a refactor adding #[serde(default)] anywhere would
        // silently let malformed manifests parse with empty strings —
        // agent.id="" would bypass rejects_empty_id at the parse layer
        // (which only fires on the validation path), agent.entry=""
        // would let SubprocessRunner spawn `Command::new("")` at intent
        // dispatch, agent.runtime would default-pick a variant breaking
        // the router/runtime contract at boot. The existing validation
        // tests catch malformed values; this test pins the prior
        // contract that an *omitted* field is a parse error, not a
        // soft-defaulted value.
        const ID_VAL: &str = "x";
        const NAME_VAL: &str = "x";
        const VERSION_VAL: &str = "0.0.1";
        const RUNTIME_VAL: &str = "node";
        const ENTRY_VAL: &str = "x.js";

        let full = format!(
            "[agent]\nid = \"{ID_VAL}\"\nname = \"{NAME_VAL}\"\nversion = \"{VERSION_VAL}\"\nruntime = \"{RUNTIME_VAL}\"\nentry = \"{ENTRY_VAL}\"\n"
        );
        let parsed = Manifest::parse(&full).expect("full agent block must parse");
        assert_eq!(parsed.agent.id, ID_VAL);
        assert_eq!(parsed.agent.name, NAME_VAL);
        assert_eq!(parsed.agent.version, VERSION_VAL);
        assert_eq!(parsed.agent.runtime, Runtime::Node);
        assert_eq!(parsed.agent.entry, ENTRY_VAL);

        let cases: [(&str, &str); 5] = [
            (
                "id",
                "[agent]\nname = \"x\"\nversion = \"0.0.1\"\nruntime = \"node\"\nentry = \"x.js\"\n",
            ),
            (
                "name",
                "[agent]\nid = \"x\"\nversion = \"0.0.1\"\nruntime = \"node\"\nentry = \"x.js\"\n",
            ),
            (
                "version",
                "[agent]\nid = \"x\"\nname = \"x\"\nruntime = \"node\"\nentry = \"x.js\"\n",
            ),
            (
                "runtime",
                "[agent]\nid = \"x\"\nname = \"x\"\nversion = \"0.0.1\"\nentry = \"x.js\"\n",
            ),
            (
                "entry",
                "[agent]\nid = \"x\"\nname = \"x\"\nversion = \"0.0.1\"\nruntime = \"node\"\n",
            ),
        ];

        for (missing_field, toml_src) in cases {
            match Manifest::parse(toml_src) {
                Err(ManifestError::Parse(_)) => {}
                // `agent.entry` carries `#[serde(default)]` so the Hermes
                // runtime can omit it. Subprocess-style runtimes still must
                // reject an empty entry — the rejection moved from Parse to
                // Validation. The `agent.entry must not be empty` Validation
                // error keeps Command::new("") out of SubprocessRunner.
                Err(ManifestError::Validation(msg))
                    if missing_field == "entry"
                        && msg.contains("agent.entry must not be empty") => {}
                other => panic!(
                    "omitting agent.{missing_field} must fail with \
                     ManifestError::Parse (or Validation for entry on \
                     subprocess runtimes) — a refactor that added \
                     #[serde(default)] to agent.{missing_field} on a \
                     subprocess-style runtime would silently let \
                     `Command::new('')` reach the runner; got {other:?}",
                ),
            }
        }
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
