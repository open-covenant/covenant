//! Live integration test for the trusted-local runner's execution-timeout
//! boundary.
//!
//! Spawns a real agent subprocess that consumes its intent and then sleeps far
//! past its `cpu_ms_per_task` deadline without ever writing stdout. The runner
//! wraps the agent stdout read in `tokio::time::timeout`; on expiry it kills the
//! child and returns `RunnerError::Timeout`. That deadline is the only thing
//! that stops a hung or runaway agent from holding a dispatch slot for its full
//! runtime, but it is pinned only by an in-process unit test that asserts the
//! error variant alone — never that the return is prompt, nor that the spawned
//! OS process does not outlive the dispatch.
//!
//! This test pins both: a regression that awaited the child before the timeout
//! (returning only after the agent's full 30 s runtime) fails the promptness
//! assertion, and a regression that leaked the process fails the liveness
//! assertion.
//!
//! Hermetic: tempdir package, local POSIX-shell fixture, no external services.
//! Run with `cargo test -p covenant-runtime --test live_timeout_kill -- --ignored live_`.

use covenant_manifest::Manifest;
use covenant_router::AgentCard;
use covenant_runtime::{Runner, RunnerError, SubprocessRunner};
use covenant_types::{AgentId, Intent, Priority};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// 2 s deadline; the agent sleeps 30 s, so the timeout is the only way the
/// dispatch can return. The deadline is generous enough that the fixture
/// reliably records its pid before the kill, yet far below the agent's runtime
/// so a regression that waited the agent out is unambiguous.
fn slow_card(package_dir: std::path::PathBuf) -> AgentCard {
    let manifest = Manifest::parse(
        r#"
[agent]
id = "slowloris"
name = "Slow Loris"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 2000
"#,
    )
    .unwrap();
    AgentCard::from_manifest_and_dir(manifest, package_dir)
}

fn intent() -> Intent {
    Intent {
        id: Uuid::nil(),
        text: "sleep past the deadline".into(),
        issuer: AgentId::new("operator@local", [9u8; 32]),
        issued_at: 0,
        priority: Priority::Normal,
        parent: None,
    }
}

#[cfg(unix)]
fn is_alive(pid: i64) -> bool {
    // `kill -0` probes for the process's existence without signalling it: exit 0
    // while it is alive, non-zero (ESRCH) once it is gone. Same user as the
    // child, so EPERM cannot mask a live process here.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
#[ignore = "live: spawns a trusted-local agent subprocess that sleeps past its deadline"]
async fn live_subprocess_runner_abandons_and_kills_an_agent_past_its_deadline() {
    if !cfg!(unix) {
        eprintln!("live_timeout_kill: skip: requires a Unix-like filesystem and POSIX shell");
        return;
    }

    let package = tempfile::tempdir().unwrap();
    let agent = package.path().join("agent.sh");
    // Consume stdin, record this process's pid for the liveness check, then
    // `exec sleep` so the process the runner tracks and kills IS the long
    // sleeper — not a shell whose `sleep` child would be orphaned by the kill.
    // stdout is never written or closed, so the runner's read blocks until the
    // `cpu_ms_per_task` deadline fires.
    std::fs::write(
        &agent,
        "#!/bin/sh\n\
         cat >/dev/null\n\
         echo $$ > pid\n\
         exec sleep 30\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
    }

    let card = slow_card(package.path().to_path_buf());

    let started = Instant::now();
    let result = SubprocessRunner::new().run(&card, &intent()).await;
    let elapsed = started.elapsed();

    // The deadline path, not the success/parse path: a hung agent surfaces as
    // Timeout.
    assert!(
        matches!(result, Err(RunnerError::Timeout(_))),
        "a slow agent must surface as RunnerError::Timeout; got {result:?}",
    );

    // Promptness is the contract the unit test omits: the dispatch returns at
    // the ~2 s deadline, nowhere near the agent's 30 s runtime. A regression
    // that awaited the child before the timeout would push this past 30 s.
    assert!(
        elapsed < Duration::from_secs(10),
        "the runner must abandon the agent at its deadline, not wait out its \
         30 s runtime; returned after {elapsed:?}",
    );

    // The fixture recorded its own pid before sleeping; it exists once the
    // dispatch has run.
    let pid: i64 = std::fs::read_to_string(package.path().join("pid"))
        .expect("the agent must have recorded its pid before sleeping")
        .trim()
        .parse()
        .expect("the recorded pid must be an integer");

    // End-to-end no-leak property: a timed-out agent's process does not outlive
    // its dispatch. The timeout path kills (and reaps) the child before
    // returning, so it is already gone; a short bounded poll absorbs any reap
    // latency. A regression that left the process running would keep `kill -0`
    // succeeding until the test fails.
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(3);
        while is_alive(pid) {
            if Instant::now() >= deadline {
                panic!("the timed-out agent process (pid {pid}) outlived its dispatch");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
