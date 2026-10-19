//! Exercise the shipped entrypoint with no HOME, coordinator, or buyer identity.
use std::process::{Command, Output};

use covenant_compute_buyer::audit::{DOCUMENTS, UPSTREAM_COMMIT};
use serde_json::Value;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env_clear()
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_reads_and_exports_the_complete_skill_offline() {
    let output = run(&["audit-skill", "--json"]);
    assert!(output.status.success(), "{:?}", output);
    let read: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(read["upstream"]["commit"], UPSTREAM_COMMIT);
    assert_eq!(read["audit_executed"], false);
    assert_eq!(read["spends_funds"], false);
    assert_eq!(read["document"], "SKILL.md");

    let list = run(&["audit-skill", "--list", "--json"]);
    assert!(list.status.success());
    let catalog: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(
        catalog["documents"].as_array().unwrap().len(),
        DOCUMENTS.len()
    );

    let read = run(&["audit-skill", "--read", "upstream/report-schema.json"]);
    assert!(read.status.success());
    assert!(serde_json::from_slice::<Value>(&read.stdout).is_ok());

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let destination = root.join(".agents/skills/covenant-compute-security-audit");
    let path = destination.to_str().unwrap();
    let output = run(&["audit-skill", "--export", path, "--json"]);
    assert!(output.status.success(), "{:?}", output);
    let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(exported["exported_to"], path);
    for asset in DOCUMENTS {
        assert_eq!(
            std::fs::read(destination.join(asset.name)).unwrap(),
            asset.content.as_bytes()
        );
    }
    std::fs::write(destination.join("SKILL.md"), "local edits").unwrap();
    assert!(!run(&["audit-skill", "--export", path]).status.success());
    assert_eq!(
        std::fs::read_to_string(destination.join("SKILL.md")).unwrap(),
        "local edits"
    );
    assert!(!root.join(".covenant-compute-mcp").exists());
}

#[test]
fn cli_refuses_unknown_flags_and_host_file_reads() {
    for args in [
        vec!["audit-skill", "--export"],
        vec!["audit-skill", "--read"],
        vec!["audit-skill", "--read", "../../etc/passwd"],
        vec!["audit-skill", "--list", "--export", "unused"],
        vec!["audit-skill", "--price", "1"],
        vec!["audit-skill", "--repo", "/tmp/target"],
    ] {
        let result = run(&args);
        assert!(!result.status.success(), "accepted {args:?}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(!message.contains("COORDINATOR_URL"), "{message}");
        assert!(!message.contains("HOME"), "{message}");
    }
}
