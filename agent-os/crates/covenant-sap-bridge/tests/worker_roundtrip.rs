//! End-to-end test of the Rust → worker JSON protocol, using a shell
//! stub in place of the real TypeScript worker. Verifies the success,
//! null, and error envelopes map onto the typed bridge surface without
//! touching the network.

use covenant_sap_bridge::attestation::AuditRootAttestation;
use covenant_sap_bridge::config::{Cluster, Config};
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
