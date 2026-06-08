//! End-to-end test of the Rust → worker JSON protocol, using a shell
//! stub in place of the real `@covenant/said-bridge` TypeScript worker.
//! Verifies the success and error envelopes map onto the typed bridge
//! surface, and that a hung or silently-exiting worker surfaces a typed
//! error, without spawning the real worker or touching the network.

use std::time::Duration;

use covenant_said_bridge::config::{DEFAULT_REST_TIMEOUT, DEFAULT_WORKER_TIMEOUT};
use covenant_said_bridge::instructions::{RegisterAgentInput, ValidateWorkInput};
use covenant_said_bridge::{
    BridgeError, Cluster, Config, PaidGates, SaidBridge, DEFAULT_SAID_API_BASE_URL,
    DEFAULT_SAID_DEVNET_PROGRAM_ID,
};

/// Enabled bridge with every paid gate open so the instruction reaches
/// the worker instead of tripping the gate. `worker_command` is whatever
/// the caller wants to stand in for the real subprocess.
fn bridge_with_worker(worker_command: Vec<String>, worker_timeout: Duration) -> SaidBridge {
    let config = Config {
        enabled: true,
        cluster: Cluster::Devnet,
        program_id: DEFAULT_SAID_DEVNET_PROGRAM_ID.into(),
        rpc_url: "https://api.devnet.solana.com".into(),
        api_base_url: DEFAULT_SAID_API_BASE_URL.into(),
        paid: PaidGates {
            register: true,
            verify: true,
            anchor: true,
            validate_work: true,
        },
        worker_command,
        worker_timeout,
        rest_timeout: DEFAULT_REST_TIMEOUT,
    };
    SaidBridge::new(config).expect("bridge")
}

/// Worker stub that drains stdin and prints `envelope_json` verbatim on
/// stdout, the shape `worker::invoke` parses from the last non-empty line.
fn bridge_with_stub(envelope_json: &str) -> SaidBridge {
    let script = format!("cat >/dev/null; printf '%s' '{envelope_json}'");
    bridge_with_worker(
        vec!["sh".into(), "-c".into(), script],
        DEFAULT_WORKER_TIMEOUT,
    )
}

#[tokio::test]
async fn register_on_chain_maps_success_envelope() {
    let bridge = bridge_with_stub(
        r#"{"ok":true,"data":{"agentPda":"Agent111","owner":"Own222","signature":"sig333"}}"#,
    );
    let res = bridge
        .register_on_chain(&RegisterAgentInput {
            metadata_uri: "https://example.test/agent.json".into(),
        })
        .await
        .expect("register");
    assert_eq!(res.agent_pda, "Agent111");
    assert_eq!(res.owner, "Own222");
    assert_eq!(res.signature, "sig333");
}

#[tokio::test]
async fn get_verified_maps_success_envelope() {
    let bridge = bridge_with_stub(r#"{"ok":true,"data":{"signature":"sigV","slot":42}}"#);
    let res = bridge.get_verified().await.expect("verify");
    assert_eq!(res.signature, "sigV");
    assert_eq!(res.slot, 42);
}

#[tokio::test]
async fn get_verified_defaults_missing_slot_to_zero() {
    let bridge = bridge_with_stub(r#"{"ok":true,"data":{"signature":"sigV"}}"#);
    let res = bridge.get_verified().await.expect("verify");
    assert_eq!(res.signature, "sigV");
    assert_eq!(res.slot, 0);
}

#[tokio::test]
async fn validate_work_maps_success_envelope() {
    let bridge = bridge_with_stub(
        r#"{"ok":true,"data":{"validationPda":"Val111","validator":"Ver222","signature":"sigW"}}"#,
    );
    let res = bridge
        .validate_work(&ValidateWorkInput {
            agent: "AgentPda".into(),
            task_hash_hex: "ab".repeat(32),
            passed: true,
            evidence_uri: "https://example.test/evidence.json".into(),
        })
        .await
        .expect("validate-work");
    assert_eq!(res.validation_pda, "Val111");
    assert_eq!(res.validator, "Ver222");
    assert_eq!(res.signature, "sigW");
}

#[tokio::test]
async fn error_envelope_with_named_error_becomes_upstream() {
    // A meaningful upstream error name (a send/confirm failure or an
    // on-chain program error) is preserved on Upstream so reconciliation
    // can branch on failure class instead of string-matching a message.
    let bridge =
        bridge_with_stub(r#"{"ok":false,"error":"insufficient funds","name":"InsufficientFunds"}"#);
    let err = bridge.get_verified().await.expect_err("should fail");
    match err {
        BridgeError::Upstream { name, message } => {
            assert_eq!(name, "InsufficientFunds");
            assert_eq!(message, "insufficient funds");
        }
        other => panic!("expected Upstream, got {other:?}"),
    }
}

#[tokio::test]
async fn error_envelope_with_generic_name_stays_rest() {
    // JS's default Error.name ("Error") and a missing name carry no class
    // signal, so they collapse to Rest rather than a spurious Upstream.
    for envelope in [
        r#"{"ok":false,"error":"boom","name":"Error"}"#,
        r#"{"ok":false,"error":"boom"}"#,
    ] {
        let bridge = bridge_with_stub(envelope);
        let err = bridge.get_verified().await.expect_err("should fail");
        match err {
            BridgeError::Rest(msg) => assert_eq!(msg, "boom", "envelope {envelope}"),
            other => panic!("expected Rest for {envelope}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn error_envelope_without_message_uses_default_text() {
    let bridge = bridge_with_stub(r#"{"ok":false}"#);
    let err = bridge.get_verified().await.expect_err("should fail");
    match err {
        BridgeError::Rest(msg) => assert_eq!(msg, "worker reported an error"),
        other => panic!("expected Rest, got {other:?}"),
    }
}

#[tokio::test]
async fn worker_timeout_surfaces_timeout_error() {
    // A worker that never returns must surface as Timeout, not hang the
    // daemon; kill_on_drop reaps the sleeping child as the future drops.
    let bridge = bridge_with_worker(
        vec!["sh".into(), "-c".into(), "sleep 30".into()],
        Duration::from_millis(200),
    );
    let err = bridge.get_verified().await.expect_err("should time out");
    assert!(
        matches!(err, BridgeError::Timeout { .. }),
        "expected Timeout, got {err:?}"
    );
}

#[tokio::test]
async fn worker_silent_exit_surfaces_worker_error() {
    // A worker that drains stdin and exits cleanly with no stdout — a
    // crashed or misbuilt worker — must surface as Worker, not a decode
    // panic on an empty line.
    let bridge = bridge_with_worker(
        vec!["sh".into(), "-c".into(), "cat >/dev/null; exit 0".into()],
        DEFAULT_WORKER_TIMEOUT,
    );
    let err = bridge.get_verified().await.expect_err("should fail");
    assert!(
        matches!(err, BridgeError::Worker(ref msg) if msg.contains("no output")),
        "expected Worker(no output), got {err:?}"
    );
}
