//! Live integration test for the Linux gVisor runtime path.
//!
//! Opt in with a Linux host, `runsc`, and a rootfs containing `/bin/sh`:
//! `COVENANT_LIVE_GVISOR_ROOTFS=/path/to/rootfs cargo test -p covenant-runtime --test live_gvisor -- --ignored live_`.

use covenant_manifest::Manifest;
use covenant_router::AgentCard;
use covenant_runtime::{GvisorRunner, Runner};
use covenant_types::{AgentId, Intent, Priority};
use std::env;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

fn skip(reason: &str) {
    eprintln!("live_gvisor: skip: {reason}");
}

fn live_rootfs() -> Option<PathBuf> {
    env::var_os("COVENANT_LIVE_GVISOR_ROOTFS").map(PathBuf::from)
}

fn runsc_path() -> PathBuf {
    env::var_os("COVENANT_LIVE_RUNSC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runsc"))
}

fn assert_runsc_available(runsc: &Path) {
    let status = Command::new(runsc)
        .arg("--version")
        .status()
        .unwrap_or_else(|e| panic!("runsc is not executable at {}: {e}", runsc.display()));
    assert!(
        status.success(),
        "runsc --version failed for {} with status {status}",
        runsc.display()
    );
}

fn sandbox_card(package_dir: PathBuf) -> AgentCard {
    let manifest = Manifest::parse(
        r#"
[agent]
id = "live-gvisor"
name = "Live gVisor"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 15000
memory_mb = 128
network = "off"

[sandbox]
required = true
backend = "linux-gvisor"
filesystem = "read-only-package"
"#,
    )
    .unwrap();
    AgentCard::from_manifest_and_dir(manifest, package_dir)
}

fn intent() -> Intent {
    Intent {
        id: Uuid::nil(),
        text: "verify sandbox dispatch".into(),
        issuer: AgentId::new("operator@local", [7u8; 32]),
        issued_at: 0,
        priority: Priority::Normal,
        parent: None,
    }
}

#[tokio::test]
#[ignore = "live: skips unless Linux, runsc, and COVENANT_LIVE_GVISOR_ROOTFS are available"]
async fn live_gvisor_runner_dispatches_with_runsc() {
    if !cfg!(target_os = "linux") {
        skip("requires Linux");
        return;
    }

    #[cfg(not(unix))]
    {
        skip("requires a Unix-like filesystem");
        return;
    }

    let Some(rootfs) = live_rootfs() else {
        skip("set COVENANT_LIVE_GVISOR_ROOTFS to a rootfs containing /bin/sh");
        return;
    };
    assert!(
        rootfs.is_dir(),
        "COVENANT_LIVE_GVISOR_ROOTFS is not a directory: {}",
        rootfs.display()
    );
    assert!(
        rootfs.join("bin/sh").exists(),
        "COVENANT_LIVE_GVISOR_ROOTFS must contain /bin/sh: {}",
        rootfs.display()
    );

    let runsc = runsc_path();
    assert_runsc_available(&runsc);

    let package = tempfile::tempdir().unwrap();
    let agent = package.path().join("agent.sh");
    std::fs::write(
        &agent,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"text\":\"gvisor ok\",\"sources\":[\"linux-gvisor\"]}'\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
    }

    let scratch = tempfile::tempdir().unwrap();
    let runner = GvisorRunner::with_paths(runsc, rootfs, scratch.path());
    let result = runner
        .run(&sandbox_card(package.path().to_path_buf()), &intent())
        .await
        .unwrap();

    assert_eq!(result.text, "gvisor ok");
    assert_eq!(result.sources, ["linux-gvisor"]);
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}
