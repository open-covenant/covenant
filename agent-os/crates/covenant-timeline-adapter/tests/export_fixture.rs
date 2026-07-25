use std::process::Command;

#[test]
fn exported_engineering_run_matches_the_offline_fixture() {
    let output = Command::new(env!("CARGO_BIN_EXE_covenant-timeline-export-demo"))
        .output()
        .expect("run Covenant Timeline exporter");
    assert!(
        output.status.success(),
        "exporter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let actual: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("exporter emits JSON");
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/covenant-engineering-run.json"))
            .expect("offline fixture is JSON");

    assert_eq!(actual, expected);
}
