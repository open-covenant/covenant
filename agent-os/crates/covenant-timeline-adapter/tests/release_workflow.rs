use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use covenant_timeline_adapter::release::{state_lock_path, MAX_JSON_BYTES};
use serde_json::Value;

#[test]
fn release_timeline_survives_restart_and_reconciles_append_only() {
    let root = repository_root();
    let timeline = root.join("docs/releases/v0.1.0-alpha.1/timeline");
    let evidence = timeline.join("evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let published = required_fixture(&evidence.join("release-published.json"));
    let expected = required_fixture(&timeline.join("run.json"));
    let temporary = TestDirectory::new();
    let state = temporary.path().join("run.json");

    succeeds(run_initial(&created, &readiness, &state), "initial");
    let initial_bytes = fs::read(&state).expect("read initial state");
    let initial = parse_json(&initial_bytes);
    assert_eq!(initial["events"].as_array().unwrap().len(), 4);

    fails(
        run_initial(&created, &readiness, &state),
        "initial must not overwrite state",
    );
    assert_eq!(
        fs::read(&state).expect("read state after refused overwrite"),
        initial_bytes
    );

    succeeds(
        run_reconcile(&created, &readiness, &state, &published),
        "reconcile",
    );
    let reconciled_bytes = fs::read(&state).expect("read reconciled state");
    let reconciled = parse_json(&reconciled_bytes);
    let initial_events = initial["events"].as_array().unwrap();
    let reconciled_events = reconciled["events"].as_array().unwrap();
    assert_eq!(reconciled_events.len(), 6);
    assert_eq!(&reconciled_events[..4], initial_events.as_slice());

    fails(
        run_reconcile(&created, &readiness, &state, &published),
        "duplicate reconciliation must fail",
    );
    assert_eq!(
        fs::read(&state).expect("read state after duplicate reconciliation"),
        reconciled_bytes
    );

    let expected_bytes = fs::read(expected).expect("read expected run fixture");
    assert!(
        reconciled_bytes == expected_bytes,
        "generated state differs from the checked run fixture"
    );
    let expected = parse_json(&expected_bytes);
    assert_eq!(reconciled, expected);
}

#[test]
fn reconciliation_rejects_tampered_coordinates_and_digests() {
    let root = repository_root();
    let evidence = root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let published = required_fixture(&evidence.join("release-published.json"));
    let temporary = TestDirectory::new();
    for kind in ["coordinate", "digest"] {
        let state = temporary.path().join(format!("{kind}.json"));
        succeeds(run_initial(&created, &readiness, &state), "initial");
        let mut run = parse_json(&fs::read(&state).expect("read initial state"));
        if kind == "coordinate" {
            run["events"][2]["assertion"]["coordinate"]["minimum"] =
                Value::from(1_779_957_192_001_i64);
        } else {
            run["events"][2]["assertion"]["evidenceRefs"][0] =
                Value::String(format!("sha256:{}", "0".repeat(64)));
        }
        fs::write(
            &state,
            serde_json::to_vec_pretty(&run).expect("serialize tampered state"),
        )
        .expect("write tampered state");
        let before = fs::read(&state).expect("read tampered state");

        fails(
            run_reconcile(&created, &readiness, &state, &published),
            "tampered state must fail",
        );
        assert_eq!(
            fs::read(&state).expect("read state after refused reconciliation"),
            before
        );
    }
}

#[test]
fn initialization_rejects_tag_commit_mismatches() {
    let root = repository_root();
    let evidence = root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let temporary = TestDirectory::new();

    for (kind, expected_error) in [
        ("shape", "tagCommit must be a 40-character lowercase hex id"),
        ("fact", "readiness commit does not match tagCommit"),
        ("identity", "identify different releases or tagged commits"),
    ] {
        let mut observation =
            parse_json(&fs::read(&readiness).expect("read readiness observation"));
        match kind {
            "shape" => observation["tagCommit"] = Value::String("ABC".into()),
            "fact" => observation["fact"]["commit"] = Value::String("0".repeat(40)),
            "identity" => {
                observation["tagCommit"] = Value::String("0".repeat(40));
                observation["fact"]["commit"] = Value::String("0".repeat(40));
            }
            _ => unreachable!(),
        }
        let path = temporary.path().join(format!("readiness-{kind}.json"));
        write_json(&path, &observation);
        let state = temporary.path().join(format!("state-{kind}.json"));

        let output = run_initial(&created, &path, &state);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "unexpected {kind} mismatch error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!state.exists(), "{kind} mismatch created state");
        assert_lock_released(&state);
    }
}

#[test]
fn initialization_rejects_oversized_json_before_parsing() {
    let root = repository_root();
    let readiness = required_fixture(
        &root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence/readiness-recorded.json"),
    );
    let temporary = TestDirectory::new();
    let oversized = temporary.path().join("oversized.json");
    fs::write(&oversized, vec![b' '; MAX_JSON_BYTES + 1]).expect("write oversized observation");
    let state = temporary.path().join("run.json");

    let output = run_initial(&oversized, &readiness, &state);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("exceeds the 1048576-byte JSON input limit"),
        "unexpected oversized-input error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!state.exists(), "oversized input created state");
    assert_lock_released(&state);
}

#[test]
fn concurrent_initialization_has_one_winner() {
    let root = repository_root();
    let evidence = root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let temporary = TestDirectory::new();
    let state = temporary.path().join("run.json");

    let first = spawn_initial(&created, &readiness, &state);
    let second = spawn_initial(&created, &readiness, &state);
    let outputs = [
        wait_for(first, "first initial"),
        wait_for(second, "second initial"),
    ];
    let winner = one_winner(&outputs);
    let error = String::from_utf8_lossy(&outputs[1 - winner].stderr);
    assert!(
        error.contains("locked by another process") || error.contains("state already exists"),
        "concurrent initial loser did not fail clearly: {error}"
    );
    assert_eq!(
        parse_json(&fs::read(&state).expect("read initialized state"))["events"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_lock_released(&state);
}

#[test]
fn concurrent_duplicate_reconciliations_have_one_winner() {
    let root = repository_root();
    let timeline = root.join("docs/releases/v0.1.0-alpha.1/timeline");
    let evidence = timeline.join("evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let published = required_fixture(&evidence.join("release-published.json"));
    let expected = required_fixture(&timeline.join("run.json"));
    let temporary = TestDirectory::new();
    let state = temporary.path().join("run.json");
    succeeds(run_initial(&created, &readiness, &state), "initial");

    let first = spawn_reconcile(&created, &readiness, &state, &published);
    let second = spawn_reconcile(&created, &readiness, &state, &published);
    let outputs = [
        wait_for(first, "first duplicate reconcile"),
        wait_for(second, "second duplicate reconcile"),
    ];
    let winner = one_winner(&outputs);
    clear_concurrent_failure(&outputs[1 - winner]);

    assert!(
        fs::read(&state).expect("read concurrent reconciliation")
            == fs::read(expected).expect("read expected run"),
        "concurrent winner differs from the checked run fixture"
    );
    assert_lock_released(&state);
}

#[test]
fn conflicting_concurrent_reconciliations_persist_only_the_winner() {
    let root = repository_root();
    let evidence = root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let published = required_fixture(&evidence.join("release-published.json"));
    let temporary = TestDirectory::new();
    let conflicting = temporary.path().join("release-published-conflicting.json");
    let mut conflicting_observation =
        parse_json(&fs::read(&published).expect("read publication observation"));
    conflicting_observation["fact"]["occurredAt"] = Value::String("2026-05-28T08:35:46Z".into());
    conflicting_observation["fact"]["coordinateMs"] = Value::from(1_779_957_346_000_i64);
    let mut conflicting_bytes =
        serde_json::to_vec_pretty(&conflicting_observation).expect("serialize publication");
    conflicting_bytes.push(b'\n');
    fs::write(&conflicting, conflicting_bytes).expect("write conflicting publication");

    let state = temporary.path().join("run.json");
    succeeds(run_initial(&created, &readiness, &state), "initial");
    let first = spawn_reconcile(&created, &readiness, &state, &published);
    let second = spawn_reconcile(&created, &readiness, &state, &conflicting);
    let outputs = [
        wait_for(first, "authoritative reconcile"),
        wait_for(second, "conflicting reconcile"),
    ];
    let winner = one_winner(&outputs);
    clear_concurrent_failure(&outputs[1 - winner]);

    let winning_publication = if winner == 0 {
        &published
    } else {
        &conflicting
    };
    let expected_state = temporary.path().join("expected.json");
    succeeds(
        run_initial(&created, &readiness, &expected_state),
        "expected initial",
    );
    succeeds(
        run_reconcile(&created, &readiness, &expected_state, winning_publication),
        "expected reconcile",
    );
    assert_eq!(
        fs::read(&state).expect("read winning state"),
        fs::read(&expected_state).expect("read expected winning state")
    );
    assert_lock_released(&state);
    assert_lock_released(&expected_state);
}

#[test]
fn reconciliation_reports_a_busy_sibling_lock() {
    let root = repository_root();
    let evidence = root.join("docs/releases/v0.1.0-alpha.1/timeline/evidence");
    let created = required_fixture(&evidence.join("release-created.json"));
    let readiness = required_fixture(&evidence.join("readiness-recorded.json"));
    let published = required_fixture(&evidence.join("release-published.json"));
    let temporary = TestDirectory::new();
    let state = temporary.path().join("run.json");
    succeeds(run_initial(&created, &readiness, &state), "initial");

    let lock_path = state_lock_path(&state);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open sibling lock");
    lock.try_lock().expect("hold sibling lock");
    let output = run_reconcile(&created, &readiness, &state, &published);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("locked by another process"),
        "unexpected busy error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    File::unlock(&lock).expect("release sibling lock");
    assert_eq!(
        parse_json(&fs::read(&state).expect("read initial state"))["events"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_lock_released(&state);
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("adapter crate is nested under agent-os/crates")
        .to_path_buf()
}

fn required_fixture(path: &Path) -> PathBuf {
    assert!(
        path.is_file(),
        "release workflow fixture is required but missing: {}",
        path.display()
    );
    path.to_path_buf()
}

fn run_initial(created: &Path, readiness: &Path, state: &Path) -> Output {
    initial_command(created, readiness, state)
        .output()
        .expect("spawn initial release process")
}

fn spawn_initial(created: &Path, readiness: &Path, state: &Path) -> Child {
    initial_command(created, readiness, state)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concurrent initial process")
}

fn initial_command(created: &Path, readiness: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_covenant-timeline-release"));
    command
        .arg("initial")
        .arg("--created")
        .arg(created)
        .arg("--readiness")
        .arg(readiness)
        .arg("--state")
        .arg(state);
    command
}

fn run_reconcile(created: &Path, readiness: &Path, state: &Path, published: &Path) -> Output {
    reconcile_command(created, readiness, state, published)
        .output()
        .expect("spawn reconcile release process")
}

fn spawn_reconcile(created: &Path, readiness: &Path, state: &Path, published: &Path) -> Child {
    reconcile_command(created, readiness, state, published)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concurrent reconcile process")
}

fn reconcile_command(created: &Path, readiness: &Path, state: &Path, published: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_covenant-timeline-release"));
    command
        .arg("reconcile")
        .arg("--created")
        .arg(created)
        .arg("--readiness")
        .arg(readiness)
        .arg("--state")
        .arg(state)
        .arg("--published")
        .arg(published);
    command
}

fn wait_for(child: Child, operation: &str) -> Output {
    child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("{operation}: wait for process: {error}"))
}

fn one_winner(outputs: &[Output; 2]) -> usize {
    let winners = outputs
        .iter()
        .enumerate()
        .filter_map(|(index, output)| output.status.success().then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(
        winners.len(),
        1,
        "expected one winner; stderr was {:?}",
        outputs
            .iter()
            .map(|output| String::from_utf8_lossy(&output.stderr))
            .collect::<Vec<_>>()
    );
    winners[0]
}

fn clear_concurrent_failure(output: &Output) {
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("locked by another process")
            || error.contains("already been reconciled")
            || error.contains("reconciliation events do not match"),
        "concurrent loser did not fail clearly: {error}"
    );
}

fn assert_lock_released(state: &Path) {
    let path = state_lock_path(state);
    assert!(path.is_file(), "persistent sibling lock is missing");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open persistent sibling lock");
    lock.try_lock().expect("sibling lock was not released");
    File::unlock(&lock).expect("release sibling lock after assertion");
}

fn succeeds(output: Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fails(output: Output, operation: &str) {
    assert!(
        !output.status.success(),
        "{operation}: process unexpectedly succeeded"
    );
    assert!(
        !output.stderr.is_empty(),
        "{operation}: process failed without an error"
    );
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("state is valid JSON")
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).expect("serialize JSON fixture");
    bytes.push(b'\n');
    fs::write(path, bytes).expect("write JSON fixture");
}

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        for _ in 0..1_000 {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "covenant-timeline-release-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temporary state directory: {error}"),
            }
        }
        panic!("could not allocate a temporary state directory");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove temporary state directory");
    }
}
