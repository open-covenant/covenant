//! Live integration test for agent environment isolation on the trusted-local runtime.
//!
//! Spawns a real agent subprocess and inspects the environment it actually
//! inherited. The pure-function allowlist (`filter_agent_env`) is unit-tested
//! over a synthetic iterator, but the `env_clear().envs(agent_subprocess_env())`
//! wiring on the real spawn path is the security boundary: it keeps operator
//! secrets out of an agent's environment so a compromised agent cannot route
//! around the capability-gated, audited secret broker. A regression that
//! dropped `.env_clear()` (leaking every operator secret) or dropped
//! `.envs(agent_subprocess_env())` (clearing everything) would pass every
//! existing test — this one fails on either.
//!
//! Hermetic: tempdir package, local shell fixture, no external services.
//! Run with `cargo test -p covenant-runtime --test live_env_isolation -- --ignored live_`.

use covenant_manifest::Manifest;
use covenant_router::AgentCard;
use covenant_runtime::{Runner, SubprocessRunner};
use covenant_types::{AgentId, Intent, Priority};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;

/// A name outside the PATH/HOME/COVENANT_* allowlist: the runner must strip it.
const OPERATOR_SECRET_KEY: &str = "OPERATOR_SECRET";
const OPERATOR_SECRET_VALUE: &str = "must-not-leak-7f3a2c";

/// A COVENANT_-prefixed name: the allowlist must pass it through verbatim.
const MARKER_KEY: &str = "COVENANT_TEST_MARKER";
const MARKER_VALUE: &str = "covenant-marker-7f3a2c";

fn probe_card(package_dir: std::path::PathBuf) -> AgentCard {
    let manifest = Manifest::parse(
        r#"
[agent]
id = "envprobe"
name = "Env Probe"
version = "0.0.1"
runtime = "rust-bin"
entry = "./agent.sh"

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
        text: "verify agent environment isolation".into(),
        issuer: AgentId::new("operator@local", [8u8; 32]),
        issued_at: 0,
        priority: Priority::Normal,
        parent: None,
    }
}

#[tokio::test]
#[ignore = "live: spawns a trusted-local agent subprocess and inspects its inherited environment"]
async fn live_subprocess_runner_withholds_operator_secret_but_inherits_allowlisted_env() {
    if !cfg!(unix) {
        eprintln!("live_env_isolation: skip: requires a Unix-like filesystem and POSIX shell");
        return;
    }

    // The runner reads the spawner's own environment, so the probe vars must be
    // set in this process before the spawn. This test lives in its own binary
    // with a single test function, so the set_var here cannot race a sibling.
    std::env::set_var(OPERATOR_SECRET_KEY, OPERATOR_SECRET_VALUE);
    std::env::set_var(MARKER_KEY, MARKER_VALUE);

    let package = tempfile::tempdir().unwrap();
    let agent = package.path().join("agent.sh");
    std::fs::write(
        &agent,
        // Consume stdin, then report what the child actually inherited as a
        // valid AgentResult document. `:-ABSENT` distinguishes a stripped var
        // from a leaked one; the values are alphanumeric so the JSON stays well
        // formed.
        "#!/bin/sh\n\
         cat >/dev/null\n\
         secret=\"${OPERATOR_SECRET:-ABSENT}\"\n\
         marker=\"${COVENANT_TEST_MARKER:-ABSENT}\"\n\
         path=\"${PATH:-ABSENT}\"\n\
         printf '{\"text\":\"env-isolation-probe\",\"sources\":[\"secret=%s\",\"marker=%s\",\"path=%s\"]}\\n' \
         \"$secret\" \"$marker\" \"$path\"\n",
    )
    .unwrap();

    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
    }

    let card = probe_card(package.path().to_path_buf());
    let result = SubprocessRunner::new()
        .run(&card, &intent())
        .await
        .expect("env-probe agent should run and emit a valid AgentResult");

    // The non-allowlisted operator secret must not reach the child: the security
    // boundary. Without env_clear() this leaks the real value and fails here.
    assert_eq!(
        result.sources[0], "secret=ABSENT",
        "operator secret {OPERATOR_SECRET_KEY} leaked into the agent environment; \
         got {:?}. The runner must strip every key outside the PATH/HOME/COVENANT_* \
         allowlist via env_clear() + agent_subprocess_env().",
        result.sources[0],
    );

    // The allowlisted COVENANT_ marker must reach the child verbatim. This is
    // what proves the pass is not vacuous: a runner that cleared the environment
    // outright (dropping .envs(agent_subprocess_env())) would also strip the
    // secret, but the shell cannot synthesize COVENANT_TEST_MARKER, so it would
    // report ABSENT here.
    assert_eq!(
        result.sources[1],
        format!("marker={MARKER_VALUE}"),
        "the allowlisted COVENANT_ marker was not inherited verbatim; got {:?}. \
         is_inheritable_agent_env_key must pass COVENANT_-prefixed keys through.",
        result.sources[1],
    );

    // PATH must be inherited verbatim so interpreters resolve. Comparing the
    // value (not just presence) keeps this meaningful: /bin/sh injects a
    // compiled-in default PATH under an empty environment, so a dropped-.envs()
    // regression would still report PATH set — but not the spawner's exact PATH.
    let spawner_path = std::env::var("PATH").expect("the test process must have PATH set");
    assert_eq!(
        result.sources[2],
        format!("path={spawner_path}"),
        "PATH was not inherited verbatim; got {:?}, expected the spawner's PATH. \
         The agent allowlist must forward PATH so python3/node interpreters resolve.",
        result.sources[2],
    );
}
