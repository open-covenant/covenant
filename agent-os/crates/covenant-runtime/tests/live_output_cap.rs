//! Live integration test for the trusted-local runner's stdout output-cap
//! boundary.
//!
//! Spawns a real agent subprocess that consumes its intent and then floods
//! stdout without bound (`exec cat /dev/zero`). The runner reads at most
//! `MAX_AGENT_OUTPUT_BYTES` (16 MiB) via `take(max + 1)`; when the stream has
//! more to give it kills the child and returns `RunnerError::OutputTooLarge`.
//! That cap is the memory-axis sibling of the execution-timeout deadline — the
//! only thing stopping a runaway or hostile agent from flooding stdout until it
//! exhausts the daemon worker's memory — but it is pinned only by an in-process
//! unit test that sets a 64-byte cap through the `#[cfg(test)]`-only
//! `with_max_output_bytes` seam, floods a bounded amount from a non-`exec`
//! fixture, and asserts the error variant alone — never that the cap is the
//! real production constant, that the return is prompt, nor that the spawned OS
//! process does not outlive the dispatch.
//!
//! This test pins all three against the default 16 MiB cap: a regression that
//! dropped the `take(max + 1)` bound would read `/dev/zero` until the
//! `cpu_ms_per_task` timeout and surface `Timeout` (failing the variant and
//! promptness assertions), and a regression that leaked the process fails the
//! liveness assertion.
//!
//! Hermetic: tempdir package, local POSIX-shell fixture, no external services.
//! Run with `cargo test -p covenant-runtime --test live_output_cap -- --ignored live_`.

use covenant_manifest::Manifest;
use covenant_router::AgentCard;
use covenant_runtime::{Runner, RunnerError, SubprocessRunner};
use covenant_types::{AgentId, Intent, Priority};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// The default cap the production runner enforces (`MAX_AGENT_OUTPUT_BYTES`).
/// Hardcoded here because the constant is crate-private; the test asserts the
/// surfaced limit equals it so a drift in the production value is caught.
const EXPECTED_CAP: usize = 16 * 1024 * 1024;

/// 30 s deadline; the read of ~16 MiB completes in well under a second, so the
/// cap — not the timeout — is what terminates the dispatch. The deadline is
/// generous enough that the fixture reliably records its pid and the runner
/// reaches the overflow arm, yet finite so a regression that dropped the cap
/// surfaces as `Timeout` here rather than hanging the test forever.
fn flood_card(package_dir: std::path::PathBuf) -> AgentCard {
    let manifest = Manifest::parse(
        r#"
[agent]
id = "firehose"
name = "Firehose"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

[resources]
cpu_ms_per_task = 30000
"#,
    )
    .unwrap();
    AgentCard::from_manifest_and_dir(manifest, package_dir)
}

fn intent() -> Intent {
    Intent {
        id: Uuid::nil(),
        text: "flood stdout past the cap".into(),
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
#[ignore = "live: spawns a trusted-local agent subprocess that floods stdout past the 16 MiB cap"]
async fn live_subprocess_runner_kills_an_agent_flooding_stdout_past_the_cap() {
    if !cfg!(unix) {
        eprintln!("live_output_cap: skip: requires a Unix-like filesystem and POSIX shell");
        return;
    }

    let package = tempfile::tempdir().unwrap();
    let agent = package.path().join("agent.sh");
    // Consume stdin, record this process's pid for the liveness check, then
    // `exec cat /dev/zero` so (a) the process the runner tracks and kills IS the
    // flooding process — not a shell whose `cat` child would be orphaned by the
    // kill — and (b) the flood is unbounded, so the runner's 16 MiB cap is the
    // only thing that terminates the read.
    std::fs::write(
        &agent,
        "#!/bin/sh\n\
         cat >/dev/null\n\
         echo $$ > pid\n\
         exec cat /dev/zero\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
    }

    let card = flood_card(package.path().to_path_buf());

    let started = Instant::now();
    // Default runner: the real 16 MiB cap, not the #[cfg(test)] 64-byte seam.
    let result = SubprocessRunner::new().run(&card, &intent()).await;
    let elapsed = started.elapsed();

    // The overflow path, not the success/parse or timeout path: a flooding agent
    // surfaces as OutputTooLarge carrying the production cap verbatim. A
    // regression dropping the take(max + 1) bound would read /dev/zero until the
    // deadline and surface Timeout instead.
    match &result {
        Err(RunnerError::OutputTooLarge { limit }) => assert_eq!(
            *limit, EXPECTED_CAP,
            "OutputTooLarge must carry the production cap; got limit={limit}",
        ),
        other => panic!("a flooding agent must surface as RunnerError::OutputTooLarge; got {other:?}"),
    }

    // Promptness is the contract the unit test omits: the ~16 MiB read trips the
    // cap in well under a second, nowhere near the 30 s cpu budget. A regression
    // that dropped the cap would read until the deadline and push this past 30 s.
    assert!(
        elapsed < Duration::from_secs(10),
        "the runner must trip the output cap promptly, not read until the 30 s \
         deadline; returned after {elapsed:?}",
    );

    // The fixture recorded its own pid before flooding; it exists once the
    // dispatch has run.
    let pid: i64 = std::fs::read_to_string(package.path().join("pid"))
        .expect("the agent must have recorded its pid before flooding")
        .trim()
        .parse()
        .expect("the recorded pid must be an integer");

    // End-to-end no-leak property: an agent killed for flooding does not outlive
    // its dispatch. The overflow path kills (and reaps) the child before
    // returning, so it is already gone; a short bounded poll absorbs any reap
    // latency. A regression that left the process running would keep `kill -0`
    // succeeding until the test fails.
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(3);
        while is_alive(pid) {
            if Instant::now() >= deadline {
                panic!("the over-cap agent process (pid {pid}) outlived its dispatch");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
