//! Offline, version-pinned security-audit instructions. Reading or exporting
//! these files does not run an audit, create a buyer, or purchase work.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use covenant_mcp::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::BuyerError;
pub use crate::AUDIT_TOOL;

pub const UPSTREAM_COMMIT: &str = "c1c8a8c1471069fb0e188eeaff69b8e8db6564a8";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/cloudflare/security-audit-skill";

pub struct AuditDocument {
    pub name: &'static str,
    pub content: &'static str,
}

macro_rules! documents {
    ($($name:literal),+ $(,)?) => {
        &[$(AuditDocument {
            name: $name,
            content: include_str!(concat!("../skills/security-audit/", $name)),
        }),+]
    };
}

/// Only these compile-time assets may be read. No request is resolved against
/// the host filesystem, target repository, URL, or process environment.
pub const DOCUMENTS: &[AuditDocument] = documents![
    "SKILL.md",
    "UPSTREAM.json",
    "upstream/LICENSE",
    "upstream/SKILL.md",
    "upstream/RECONNAISSANCE.md",
    "upstream/HUNTING.md",
    "upstream/VALIDATION-AND-REPORTING.md",
    "upstream/ATTACK-CLASSES.md",
    "upstream/AI-AND-LLM.md",
    "upstream/CLIENT-SIDE.md",
    "upstream/CLOUD-AND-DEPLOYMENT.md",
    "upstream/DATA-ISOLATION-AND-LIFECYCLE.md",
    "upstream/DESKTOP-MOBILE-AND-LOCAL-IPC.md",
    "upstream/MEMORY-SAFETY-AND-BINARY.md",
    "upstream/PROTOCOLS-RPC-AND-MESSAGING.md",
    "upstream/RESOURCE-EXHAUSTION-AND-AVAILABILITY.md",
    "upstream/SUPPLY-CHAIN-AND-RELEASE.md",
    "upstream/WEB-PROTOCOL-AND-AUTH.md",
    "upstream/report-schema.json",
    "upstream/validate-findings.cjs",
    "upstream/validate-findings.test.cjs",
    "upstream/validate-coverage-ledger.cjs",
    "upstream/validate-coverage-ledger.test.cjs",
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditArgs {
    /// A name from the bundled document index; defaults to the Covenant entrypoint.
    #[serde(default)]
    pub document: Option<String>,
}

pub fn document(name: &str) -> Result<&'static AuditDocument, BuyerError> {
    DOCUMENTS
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| BuyerError::Protocol("unknown bundled audit document; use audit-skill --list or the compute.audit document index".into()))
}

pub fn catalog() -> Value {
    json!({
        "kind": "bundled_security_audit_guidance",
        "upstream": {"repository": UPSTREAM_REPOSITORY, "commit": UPSTREAM_COMMIT, "license": "MIT"},
        "audit_executed": false,
        "spends_funds": false,
        "documents": DOCUMENTS.iter().map(|asset| json!({
            "name": asset.name,
            "bytes": asset.content.len(),
            "sha256": content_hash(asset.content),
        })).collect::<Vec<_>>(),
    })
}

fn content_hash(content: &str) -> String {
    Sha256::digest(content.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn read_document(args: &AuditArgs) -> Result<Value, BuyerError> {
    let asset = document(args.document.as_deref().unwrap_or("SKILL.md"))?;
    let mut result = catalog();
    result["document"] = json!(asset.name);
    result["content"] = json!(asset.content);
    Ok(result)
}

pub fn tool_spec() -> ToolSpec {
    ToolSpec {
        name: AUDIT_TOOL.into(),
        description: "Read the bundled Cloudflare security-audit workflow and Covenant adapter. Free, local instructions for your coding agent: reconnaissance, hunting, fresh verification, structured findings and coverage reports. Does not execute an audit, access target source, dispatch jobs, or spend funds. Start with no arguments, then read named documents from the returned index. Export the skill and its Node.js validators with covenant-compute audit-skill --export <new-directory>.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "document": {
                    "type": "string",
                    "description": "Exact bundled document name. Defaults to SKILL.md.",
                    "enum": DOCUMENTS.iter().map(|asset| asset.name).collect::<Vec<_>>(),
                    "default": "SKILL.md"
                }
            },
            "additionalProperties": false
        }),
    }
}

/// Export trusted bundled files into a newly created directory. Existing
/// installations are never overwritten. Ancestors must be ordinary directories;
/// this installer is for a user-owned path, not a hostile shared directory.
pub fn export_skill(destination: &Path) -> std::io::Result<PathBuf> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    if destination.as_os_str().is_empty()
        || destination
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid(
            "export requires a new directory without '..' components",
        ));
    }
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        std::env::current_dir()?.join(destination)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("export requires a new directory"))?;
    let mut ancestor = PathBuf::new();
    for part in parent.components() {
        ancestor.push(part.as_os_str());
        match fs::symlink_metadata(&ancestor) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(invalid(
                    "export ancestors must be directories, not symlinks",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&ancestor)?;
            }
            Err(error) => return Err(error),
        }
    }
    // create_dir is exclusive even when the destination is a dangling symlink.
    fs::create_dir(&absolute)?;
    fs::create_dir(absolute.join("upstream"))?;
    for asset in DOCUMENTS {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(absolute.join(asset.name))?;
        file.write_all(asset.content.as_bytes())?;
    }
    Ok(absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_pins_every_upstream_asset() {
        let manifest: Value =
            serde_json::from_str(document("UPSTREAM.json").unwrap().content).unwrap();
        assert_eq!(manifest["commit"], UPSTREAM_COMMIT);
        assert_eq!(manifest["repository"], UPSTREAM_REPOSITORY);
        assert_eq!(manifest["license"], "MIT");
        let files = manifest["files"].as_object().unwrap();
        assert_eq!(files.len(), DOCUMENTS.len() - 2);
        for asset in DOCUMENTS
            .iter()
            .filter(|asset| asset.name.starts_with("upstream/"))
        {
            assert_eq!(files[asset.name], content_hash(asset.content));
        }
    }

    #[test]
    fn reads_are_allowlisted_and_never_normalize_untrusted_paths() {
        for name in [
            "../SKILL.md",
            "/etc/passwd",
            "upstream/../SKILL.md",
            "upstream\\SKILL.md",
            "https://example.com",
            "",
        ] {
            assert!(document(name).is_err());
        }
        assert!(serde_json::from_value::<AuditArgs>(json!({"repo":"/tmp/target"})).is_err());
        let result = read_document(&AuditArgs::default()).unwrap();
        assert_eq!(result["document"], "SKILL.md");
        assert_eq!(result["audit_executed"], false);
        assert_eq!(result["spends_funds"], false);
    }

    #[test]
    fn export_is_complete_and_refuses_existing_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let destination = root.join("skills/audit");
        assert_eq!(export_skill(&destination).unwrap(), destination);
        for asset in DOCUMENTS {
            assert_eq!(
                fs::read(destination.join(asset.name)).unwrap(),
                asset.content.as_bytes()
            );
        }
        fs::write(destination.join("SKILL.md"), "user changes").unwrap();
        assert!(export_skill(&destination).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md")).unwrap(),
            "user changes"
        );
        assert!(export_skill(&root.join("skills/../other")).is_err());
        assert!(!root.join("other").exists());
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_symlinks_and_non_directory_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("link")).unwrap();
        assert!(export_skill(&root.join("link")).is_err());
        assert!(export_skill(&root.join("link/nested")).is_err());
        fs::write(root.join("file"), "preserve").unwrap();
        assert!(export_skill(&root.join("file/nested")).is_err());
        assert!(!root.join("missing").exists());
    }
}
