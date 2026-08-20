//! Live integration test: pins the `covenant-sdk` `DenialKind::OutOfScope`
//! classification against the *real* daemon's tool-call scope-rejection
//! wording — the second arm of the SDK's `denial()` classifier, which the
//! existing `live_sdk_denial_classify_grant_retry` test does not cover (it pins
//! only the `MissingCapability` arm).
//!
//! `denial()` (covenant-sdk/src/lib.rs) splits policy denials into two kinds.
//! `MissingCapability` extracts the required action from the daemon's
//! `covenant capabilities grant <action>` hint — now live-pinned. `OutOfScope`
//! is the other branch: it fires when the caller *holds* the capability but its
//! scope does not cover the call, and the SDK detects it with a single
//! `message.contains("capability scope")`. That branch is only ever unit-tested:
//! `denial_flags_out_of_scope_without_a_capability` feeds a hardcoded fixture
//! string. The real daemon's tool-call scope rejection emits
//! `tool {name} rejected by capability scope: arguments do not match capability
//!  scope` (covenantd/src/lib.rs). If that wording ever drops the literal
//! `capability scope` substring, the unit fixture keeps passing while the live
//! SDK loop silently downgrades `OutOfScope` to the opaque `SdkError::Daemon`,
//! losing both the kind *and* the `capability: None` signal — the actionable
//! distinction that a self-grant cannot fix a scope mismatch.
//!
//! This test pins that boundary end-to-end. The operator self-grants
//! `tool.call.echo` with a scope that allows exactly `{"text":"allowed"}`; a
//! mismatched call must surface as
//! `SdkError::Denied { capability: None, kind: OutOfScope }`, and the message
//! must carry the real daemon's `capability scope` wording. A matching call
//! then succeeds — proving the scoped grant round-trips and the cap genuinely
//! exists (so the denial is `OutOfScope`, not `MissingCapability`).
//!
//! Hermetic — built-in echo tool, in-process scope enforcement, no external
//! services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_sdk_outofscope_classify -- --ignored live_`.

use covenant_sdk::{Client, Content, DenialKind, SdkError};
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().unwrap().port()
}

async fn wait_for_sock(path: &Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_operator_token(home: &Path) {
    let path = home.join("peers").join("operator.token");
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                return;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn spawn_daemon(home: &Path) -> Child {
    let port = pick_free_port();
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let child = Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");
    if !wait_for_sock(&home.join("sock")).await {
        panic!("daemon never created its socket");
    }
    wait_for_operator_token(home).await;
    child
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives covenant-sdk Client through a scoped-grant out-of-scope denial"]
async fn live_sdk_outofscope_classify_against_real_daemon() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;

    let mut client = Client::connect_with_token_file(home.path())
        .await
        .expect("SDK connects + authenticates against the real daemon");

    // Grant tool.call.echo with a scope that allows exactly {"text":"allowed"}.
    // The cap exists, so check_capabilities passes; the per-call scope check
    // (tool_call_scope_allows) then enforces the arguments.allow equality. The
    // daemon accepts this scope (validate_scope: tool namespace, version 1).
    let echo_scope = json!({
        "version": 1,
        "tool": "echo",
        "arguments": { "allow": { "text": "allowed" } },
    });
    client
        .grant_capability("tool.call.echo", Some(echo_scope), None)
        .await
        .expect("operator self-grants a scoped tool.call.echo");

    // THE pin: a call whose arguments fall outside the granted scope must be
    // rejected, and the SDK must CLASSIFY the real daemon's wording as
    // OutOfScope — not the opaque Daemon fallback. The unit fixture hardcodes
    // the "capability scope" substring; only this live round-trip catches a
    // drift between the daemon's actual wording and the SDK's contains() anchor.
    // OutOfScope carries capability: None: a self-grant cannot fix a scope
    // mismatch, unlike the MissingCapability arm where grant_capability closes
    // the loop.
    let err = client
        .call_tool("echo", json!({ "text": "denied" }))
        .await
        .expect_err("out-of-scope call_tool must be denied");
    match err {
        SdkError::Denied {
            capability,
            kind,
            message,
        } => {
            assert!(
                capability.is_none(),
                "OutOfScope must carry no grant hint (a self-grant cannot fix it), got {capability:?}"
            );
            assert_eq!(
                kind,
                DenialKind::OutOfScope,
                "expected the real daemon wording classified as OutOfScope"
            );
            assert!(
                message.contains("capability scope"),
                "message must carry the real daemon's scope-rejection wording, got: {message}"
            );
        }
        other => panic!(
            "expected Denied{{capability:None, OutOfScope}} against the real daemon wording, \
             got {other:?}"
        ),
    }

    // Contrast pin: the scoped grant genuinely took effect — a matching call
    // succeeds, proving the cap exists and only the mismatching arguments are
    // rejected. This rules out a MissingCapability mis-read (no cap) and
    // confirms the denial above was a true scope rejection.
    let out = client
        .call_tool("echo", json!({ "text": "allowed" }))
        .await
        .expect("in-scope call_tool succeeds");
    assert!(!out.is_error, "in-scope echo must not be a tool error");
    assert_eq!(out.content, vec![Content::text("allowed")]);

    // A policy denial leaves the connection usable, like any daemon error.
    client.ping().await.expect("connection survives the denial");

    drop(client);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
