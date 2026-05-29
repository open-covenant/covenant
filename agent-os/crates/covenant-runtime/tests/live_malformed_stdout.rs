//! Live integration test for malformed agent stdout on the trusted-local runtime.
//!
//! Hermetic: tempdir package, local shell fixture, no external services.
//! Run with `cargo test -p covenant-runtime --test live_malformed_stdout -- --ignored live_`.

use covenant_manifest::Manifest;
use covenant_router::AgentCard;
use covenant_runtime::{Runner, RunnerError, SubprocessRunner};
use covenant_types::{AgentId, Intent, Priority};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;

fn malformed_card(package_dir: std::path::PathBuf) -> AgentCard {
    let manifest = Manifest::parse(
        r#"
[agent]
id = "malformed"
name = "Malformed"
version = "0.0.1"
runtime = "rust-bin"
entry = "./malformed.sh"

[resources]
cpu_ms_per_task = 5000
"#,
    )
    .unwrap();
    AgentCard::from_manifest_and_dir(manifest, package_dir)
}

fn intent() -> Intent {
    Intent {
        id: Uuid::nil(),
        text: "verify malformed stdout handling".into(),
        issuer: AgentId::new("operator@local", [8u8; 32]),
        issued_at: 0,
        priority: Priority::Normal,
        parent: None,
    }
}

#[tokio::test]
#[ignore = "live: spawns a malformed trusted-local agent subprocess"]
async fn live_subprocess_runner_rejects_malformed_stdout() {
    if !cfg!(unix) {
        eprintln!("live_malformed_stdout: skip: requires a Unix-like filesystem");
        return;
    }

    let package = tempfile::tempdir().unwrap();
    let agent = package.path().join("malformed.sh");
    std::fs::write(
        &agent,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'not-json'\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
    }

    let card = malformed_card(package.path().to_path_buf());
    let result = SubprocessRunner::new().run(&card, &intent()).await;
    match result {
        Err(RunnerError::MalformedStdout { source, .. }) => {
            assert!(source.is_syntax() || source.is_data());
        }
        other => panic!("unexpected runtime result: {other:?}"),
    }
}
