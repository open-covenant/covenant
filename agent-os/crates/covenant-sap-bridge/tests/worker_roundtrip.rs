//! End-to-end test of the Rust → worker JSON protocol, using a shell
//! stub in place of the real TypeScript worker. Verifies the success,
//! null, and error envelopes map onto the typed bridge surface without
//! touching the network.

use std::time::Duration;

use covenant_sap_bridge::attestation::AuditRootAttestation;
use covenant_sap_bridge::config::{Cluster, Config, DEFAULT_WORKER_TIMEOUT};
use covenant_sap_bridge::identity::AgentManifest;
use covenant_sap_bridge::{BridgeError, SapBridge};

/// Build an enabled config whose "worker" is a shell stub: it drains
/// stdin and prints the given envelope JSON verbatim.
fn bridge_with_stub(envelope_json: &str) -> SapBridge {
    let script = format!("cat >/dev/null; printf '%s' '{envelope_json}'");
    let config = Config {
        enabled: true,
        cluster: Cluster::Devnet,
        program_id: "SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ".into(),
        rpc_url: "https://api.devnet.solana.com".into(),
        explorer_url: "https://explorer.oobeprotocol.ai".into(),
        worker_command: vec!["sh".into(), "-c".into(), script],
        worker_timeout: DEFAULT_WORKER_TIMEOUT,
    };
    SapBridge::new(config).expect("bridge")
}

fn demo_manifest() -> AgentManifest {
    AgentManifest {
        name: "covenant-demo".into(),
        protocols: vec!["a2a".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn publish_agent_maps_success_envelope() {
    let bridge =
        bridge_with_stub(r#"{"ok":true,"data":{"agentPda":"Agent111","signature":"sig222"}}"#);
    let published = bridge
        .publish_agent(&demo_manifest())
        .await
        .expect("publish");
    assert_eq!(published.agent_pda, "Agent111");
    assert_eq!(published.signature, "sig222");
}

#[tokio::test]
async fn attest_root_maps_success_envelope() {
    let bridge =
        bridge_with_stub(r#"{"ok":true,"data":{"ledgerPda":"Ledg999","signature":"sigZ"}}"#);
    let att = AuditRootAttestation {
        root_hash_hex: "00".repeat(32),
        release_target: "agent".into(),
        release_subject: "covenant-demo".into(),
        release_scope: "audit".into(),
        recorded_at: 0,
    };
    let published = bridge.publish_audit_root(&att).await.expect("attest");
    assert_eq!(published.ledger_pda, "Ledg999");
    assert_eq!(published.signature, "sigZ");
}

#[tokio::test]
async fn find_agent_null_data_is_none() {
    let bridge = bridge_with_stub(r#"{"ok":true,"data":null}"#);
    let found = bridge.find_agent_by_pda("Whatever").await.expect("lookup");
    assert!(found.is_none());
}

#[tokio::test]
async fn find_by_protocol_maps_peer_list() {
    let bridge = bridge_with_stub(
        r#"{"ok":true,"data":[{"agentPda":"A1","display":"one","protocols":["a2a"],"reputationScore":42}]}"#,
    );
    let peers = bridge.find_agents_by_protocol("a2a").await.expect("peers");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].agent_pda, "A1");
    assert_eq!(peers[0].reputation_score, Some(42));
}

#[tokio::test]
async fn error_envelope_becomes_rpc_error() {
    let bridge = bridge_with_stub(r#"{"ok":false,"error":"boom","name":"SendError"}"#);
    let err = bridge
        .publish_agent(&demo_manifest())
        .await
        .expect_err("should fail");
    match err {
        BridgeError::Rpc(msg) => assert_eq!(msg, "boom"),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

#[tokio::test]
async fn update_agent_maps_success_envelope() {
    let bridge =
        bridge_with_stub(r#"{"ok":true,"data":{"agentPda":"Agent111","signature":"upd333"}}"#);
    let published = bridge.update_agent(&demo_manifest()).await.expect("update");
    assert_eq!(published.agent_pda, "Agent111");
    assert_eq!(published.signature, "upd333");
}

#[tokio::test]
async fn describe_agent_null_data_is_none() {
    let bridge = bridge_with_stub(r#"{"ok":true,"data":null}"#);
    let detail = bridge.describe_agent("Whatever").await.expect("describe");
    assert!(detail.is_none());
}

#[tokio::test]
async fn diff_agent_against_missing_chain_account_is_none() {
    // describe returns null -> diff is None (caller should publish).
    let bridge = bridge_with_stub(r#"{"ok":true,"data":null}"#);
    let diff = bridge
        .diff_agent("MissingPda", &demo_manifest())
        .await
        .expect("diff");
    assert!(diff.is_none());
}

#[tokio::test]
async fn diff_agent_against_matching_chain_account_is_empty() {
    let envelope = r#"{"ok":true,"data":{"agentPda":"P","wallet":"W","name":"covenant-demo","description":"","capabilities":[],"pricing":[],"protocols":["a2a"],"agentId":null,"agentUri":null,"x402Endpoint":null,"isActive":true,"reputationScore":0}}"#;
    let bridge = bridge_with_stub(envelope);
    let diff = bridge
        .diff_agent("P", &demo_manifest())
        .await
        .expect("diff")
        .expect("some");
    assert!(diff.is_empty(), "{diff:?}");
}

#[tokio::test]
async fn worker_invoke_times_out_on_hung_subprocess_with_short_budget() {
    // Pre-fix: worker::invoke awaited wait_with_output with no deadline,
    // so a stalled SAP worker would block the daemon's reconciliation /
    // attestation task forever — violating the "bridge must never block
    // the offline daemon" guarantee. Post-fix: the timeout fires, the
    // subprocess is reaped via kill_on_drop, and the caller receives a
    // structured BridgeError::Timeout it can backoff distinctly from a
    // spawn / decode failure.
    //
    // Stub: drains stdin, sleeps 5s (well beyond a healthy round-trip),
    // and only THEN prints an envelope. The 200ms config budget guarantees
    // the timeout branch fires before the stub finishes.
    let script = "cat >/dev/null; sleep 5; printf '{\"ok\":true,\"data\":{}}'";
    let config = Config {
        enabled: true,
        cluster: Cluster::Devnet,
        program_id: "SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ".into(),
        rpc_url: "https://api.devnet.solana.com".into(),
        explorer_url: "https://explorer.oobeprotocol.ai".into(),
        worker_command: vec!["sh".into(), "-c".into(), script.into()],
        // Budget of 1s gives the timeout branch headroom under CI load
        // without dragging out the test (still 5x faster than the stub).
        worker_timeout: Duration::from_secs(1),
    };
    let bridge = SapBridge::new(config).expect("bridge");
    let start = std::time::Instant::now();
    let err = bridge
        .publish_agent(&demo_manifest())
        .await
        .expect_err("must time out");
    let elapsed = start.elapsed();
    // Upper bound the deadline at the stub's full sleep length minus a
    // small margin — strict enough to catch a refactor that silently
    // dropped the timeout (which would let the stub run to completion
    // and return Ok), loose enough that CI jitter (slow VM, contended
    // scheduler) doesn't flake.
    assert!(
        elapsed < Duration::from_millis(4_500),
        "timeout must fire before the stub's 5s sleep finishes — pins that the deadline \
         is real, not silently dropped; got elapsed {elapsed:?}"
    );
    match err {
        BridgeError::Timeout { secs } => {
            assert_eq!(secs, 1, "Timeout.secs must report the configured budget");
        }
        other => panic!(
            "expected BridgeError::Timeout (so reconciliation loops can apply a hang-specific \
             backoff), got {other:?}"
        ),
    }
}

#[tokio::test]
async fn attest_root_rejects_short_hash_before_spawning_worker() {
    // The hash-length pre-validation in publish_audit_root must fire
    // before any subprocess spawn. Using a worker_command that points
    // at a binary that DOES NOT EXIST means a spawn would surface
    // BridgeError::Worker — so receiving BridgeError::Invalid here
    // proves the validation ran before the spawn attempt.
    let config = Config {
        enabled: true,
        cluster: Cluster::Devnet,
        program_id: "SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ".into(),
        rpc_url: "https://api.devnet.solana.com".into(),
        explorer_url: "https://explorer.oobeprotocol.ai".into(),
        worker_command: vec!["definitely-not-a-real-binary-xyz".into()],
        worker_timeout: DEFAULT_WORKER_TIMEOUT,
    };
    let bridge = SapBridge::new(config).expect("bridge");
    let att = AuditRootAttestation {
        root_hash_hex: "deadbeef".into(), // 8 chars, not 64
        release_target: "agent".into(),
        release_subject: "covenant-demo".into(),
        release_scope: "audit".into(),
        recorded_at: 0,
    };
    let err = bridge
        .publish_audit_root(&att)
        .await
        .expect_err("must reject");
    match err {
        BridgeError::Invalid(msg) => {
            assert!(
                msg.contains("64"),
                "Invalid message must name the required length so the operator can fix \
                 the source caller: {msg}"
            );
        }
        BridgeError::Worker(_) => panic!(
            "Worker error means the bridge tried to spawn the non-existent binary instead \
             of pre-validating the hash — the validation branch did not fire"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn disabled_bridge_never_invokes_worker() {
    // Worker command would error if spawned (nonexistent program), so
    // reaching it at all would surface as a Worker error, not Disabled.
    let config = Config {
        worker_command: vec!["definitely-not-a-real-binary-xyz".into()],
        ..Config::disabled(Cluster::Devnet)
    };
    let bridge = SapBridge::new(config).expect("bridge");
    let err = bridge
        .publish_agent(&demo_manifest())
        .await
        .expect_err("disabled");
    assert!(matches!(err, BridgeError::Disabled));
}

#[tokio::test]
async fn worker_timeout_secs_env_override_is_parsed_and_applied() {
    // Pin the env-var wiring so an operator-tuned timeout actually
    // reaches the subprocess deadline. A refactor that misnamed the env
    // key, or fell back to DEFAULT_WORKER_TIMEOUT on a numeric value,
    // would surface here.
    let cfg = Config::from_env([("COVENANT_SAP_WORKER_TIMEOUT_SECS", "7")]);
    assert_eq!(cfg.worker_timeout, Duration::from_secs(7));
    // Garbage falls back to the default rather than silently disabling the
    // deadline (zero would).
    let cfg = Config::from_env([("COVENANT_SAP_WORKER_TIMEOUT_SECS", "abc")]);
    assert_eq!(cfg.worker_timeout, DEFAULT_WORKER_TIMEOUT);
    let cfg = Config::from_env([("COVENANT_SAP_WORKER_TIMEOUT_SECS", "0")]);
    assert_eq!(
        cfg.worker_timeout, DEFAULT_WORKER_TIMEOUT,
        "0 must not silently disable the deadline — fall back to the default"
    );
}
