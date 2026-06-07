use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

const SKILL_MD: &str = "---\nname: covenant\ndescription: verifiable agent execution on Solana\nmetadata:\n  version: 0.1.0\n---\n\n# covenant\n\nbody text here\n";

fn covenant_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/covenant")
        .canonicalize()
        .expect("covenant CLI binary not built; run `cargo build -p covenant` first")
}

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_operator_token(home: &std::path::Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn run_cli(cli_exe: &std::path::Path, home: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(cli_exe)
        .args(args)
        .env("COVENANT_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn covenant CLI");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "CLI failed for {args:?}: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status,
    );
    assert!(
        stderr.trim().is_empty(),
        "CLI command {args:?} must not emit stderr on success: {stderr:?}",
    );
    stdout
}

#[tokio::test]
#[ignore = "live: spawns covenantd + runs `covenant skill {add,list,show,verify}` subprocesses"]
async fn live_cli_skill_add_list_show_verify_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon_exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(daemon_exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_operator_token(home.path()).await;

    // A skill tree the daemon will install from.
    let skill_src = tempfile::tempdir().expect("skill tempdir");
    std::fs::write(skill_src.path().join("SKILL.md"), SKILL_MD).unwrap();
    std::fs::create_dir(skill_src.path().join("references")).unwrap();
    std::fs::write(
        skill_src.path().join("references/audit.md"),
        "audit witness\n",
    )
    .unwrap();
    let skill_dir = skill_src.path().to_string_lossy().to_string();

    let cli = covenant_cli_bin();

    let added: Value = serde_json::from_str(
        run_cli(
            &cli,
            home.path(),
            &[
                "skill",
                "add",
                &skill_dir,
                "--url",
                "https://github.com/open-covenant/covenant-skill/tree/v0.1.0/skill",
                "--tag",
                "v0.1.0",
                "--commit",
                "0000000000000000000000000000000000000000",
                "--json",
            ],
        )
        .await
        .trim(),
    )
    .expect("skill add --json must be JSON");
    assert_eq!(added["kind"].as_str(), Some("skill"));
    assert_eq!(added["skill"]["name"].as_str(), Some("covenant"));
    let installed_digest = added["skill"]["digest"]
        .as_str()
        .expect("digest")
        .to_string();
    assert!(installed_digest.starts_with("sha256:"));

    let listed: Value = serde_json::from_str(
        run_cli(&cli, home.path(), &["skill", "list", "--json"])
            .await
            .trim(),
    )
    .expect("skill list --json must be JSON");
    assert_eq!(listed["kind"].as_str(), Some("skill_list"));
    let skills = listed["skills"].as_array().expect("skills array");
    assert!(
        skills
            .iter()
            .any(|s| s["name"].as_str() == Some("covenant")),
        "skill list must include the installed skill: {listed:?}",
    );

    let shown: Value = serde_json::from_str(
        run_cli(&cli, home.path(), &["skill", "show", "covenant", "--json"])
            .await
            .trim(),
    )
    .expect("skill show --json must be JSON");
    assert_eq!(shown["kind"].as_str(), Some("skill"));
    assert_eq!(
        shown["skill"]["digest"].as_str(),
        Some(installed_digest.as_str())
    );

    // verify — a freshly installed, untouched skill must re-verify clean
    let verified: Value = serde_json::from_str(
        run_cli(
            &cli,
            home.path(),
            &["skill", "verify", "covenant", "--json"],
        )
        .await
        .trim(),
    )
    .expect("skill verify --json must be JSON");
    assert_eq!(verified["kind"].as_str(), Some("skill_verify"));
    assert_eq!(verified["digest_ok"].as_bool(), Some(true));
    assert_eq!(verified["name"].as_str(), Some("covenant"));

    // the install must have left a SkillInstalled provenance row in the chain
    let audit: Value = serde_json::from_str(
        run_cli(&cli, home.path(), &["audit", "recent", "--json"])
            .await
            .trim(),
    )
    .expect("audit recent --json must be JSON");
    let events = audit["events"].as_array().expect("events");
    assert!(
        events
            .iter()
            .any(|e| e["kind"]["type"].as_str() == Some("skill_installed")
                && e["kind"]["name"].as_str() == Some("covenant")),
        "installing a skill must record a skill_installed audit row: {audit:?}",
    );

    let _ = child.kill().await;
}
