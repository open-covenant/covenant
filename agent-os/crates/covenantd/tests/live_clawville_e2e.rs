//! Daemon-driven e2e for the ClawVille bounty-verification profile. Spawns
//! the real covenantd with `COVENANT_CLAWVILLE_ENABLED=true` and drives the
//! whole flow over the IPC socket:
//!
//!   ListTools → the four `clawville.bounty.*` tools are advertised.
//!   CallTool  → `clawville.bounty.verify` without a grant is rejected by
//!               the capability gate; with grants, the full flow
//!               (open → scope → verify → release) round-trips and a clean
//!               submission yields a `pass` verdict + a `release_payment`
//!               decision keyed to the buyer.
//!
//! Hermetic and deterministic — the tools are pure compute (no key, no
//! network), so unlike the metaplex e2e this is NOT `#[ignore]`'d and runs
//! in CI. Proves the full daemon path: startup env wiring → tool
//! registration → capability gate → clawville_tool_call → verdict.

use covenant_clawville::{ActionEntry, AuditTrail};
use covenant_ipc::{read_frame, write_frame, Request, Response};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

const WORKER: &str = "9sFJ95mZsBTGqTEBkcbmsx2V8RQiZ5iQACCLPLE61aWH";
const POSTER: &str = "96GsGo69kVfPZffudCexfnsSi5EuhAyd278MuJPwzGdu";

async fn wait_for_sock(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn read_operator_token(home: &std::path::Path) -> String {
    let path = home.join("peers").join("operator.token");
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn operator(sock: &std::path::Path, token_b58: &str) -> UnixStream {
    let mut stream = UnixStream::connect(sock).await.expect("connect");
    match req(&mut stream, Request::Authenticate { token_b58: token_b58.to_string() }).await {
        Response::Authenticated { .. } => stream,
        other => panic!("operator authentication failed: {other:?}"),
    }
}

async fn grant(stream: &mut UnixStream, action: &str) {
    match req(
        stream,
        Request::GrantCapability { action: action.into(), scope: None, expires_at: None },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("grant {action} failed: {other:?}"),
    }
}

async fn call(stream: &mut UnixStream, name: &str, arguments: Value) -> Value {
    match req(stream, Request::CallTool { name: name.into(), arguments }).await {
        Response::ToolResult { content, is_error } => {
            assert!(!is_error, "{name} returned tool error: {content:?}");
            let blob = serde_json::to_value(&content).unwrap();
            // content is [{ "type":"json", "value": ... }]
            blob[0]["value"].clone()
        }
        other => panic!("{name} call failed: {other:?}"),
    }
}

#[tokio::test]
async fn live_covenantd_clawville_bounty_flow_round_trips() {
    let home = tempfile::tempdir().expect("tempdir");
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("HOME", home.path())
        .env("COVENANT_CLAWVILLE_ENABLED", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        let _ = child.kill().await;
        panic!("daemon never created its socket");
    }
    let token = read_operator_token(home.path()).await;

    // ListTools: the four bounty tools are advertised.
    {
        let mut s = operator(&sock, &token).await;
        match req(&mut s, Request::ListTools).await {
            Response::ToolList { tools } => {
                let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
                for want in [
                    "clawville.bounty.open",
                    "clawville.bounty.scope",
                    "clawville.bounty.verify",
                    "clawville.bounty.release",
                    "clawville.land.grant",
                    "clawville.land.authorize",
                ] {
                    assert!(names.contains(&want), "{want} must be advertised, got {names:?}");
                }
            }
            other => panic!("ListTools failed: {other:?}"),
        }
    }

    // Capability gate: verify without a grant is rejected by name.
    {
        let mut s = operator(&sock, &token).await;
        match req(
            &mut s,
            Request::CallTool { name: "clawville.bounty.verify".into(), arguments: json!({}) },
        )
        .await
        {
            Response::Error { message } => assert!(
                message.contains("tool.call.clawville.bounty.verify"),
                "ungranted call must name the capability, got {message:?}"
            ),
            other => panic!("ungranted call must be rejected, got {other:?}"),
        }
    }

    // Full flow under grants.
    let mut s = operator(&sock, &token).await;
    for action in [
        "tool.call.clawville.bounty.open",
        "tool.call.clawville.bounty.scope",
        "tool.call.clawville.bounty.verify",
        "tool.call.clawville.bounty.release",
        "tool.call.clawville.land.grant",
        "tool.call.clawville.land.authorize",
    ] {
        grant(&mut s, action).await;
    }

    let criteria = json!({ "criteria": [ { "kind": "result_contains", "needle": "done" } ] });

    // open → pins criteria
    let opened = call(
        &mut s,
        "clawville.bounty.open",
        json!({ "bountyId": "b1", "poster": POSTER, "escrowRef": "escrow-cid-1", "criteria": criteria }),
    )
    .await;
    let criteria_hash = opened["criteriaHash"].as_str().unwrap().to_string();
    assert_eq!(criteria_hash.len(), 64);

    // scope → worker grant
    let grant_obj = call(
        &mut s,
        "clawville.bounty.scope",
        json!({ "bountyId": "b1", "worker": WORKER, "allowedActions": ["tool.call.fs"] }),
    )
    .await;

    // Build a submission whose audit root matches its trail (clean evidence).
    let trail = AuditTrail::new(vec![ActionEntry {
        seq: 0,
        action: "tool.call.fs.read".into(),
        detail_hash: "c".repeat(64),
    }]);
    let submission = json!({
        "bountyId": "b1", "worker": WORKER, "result": "task done",
        "auditRoot": trail.root(),
        "trail": serde_json::to_value(&trail).unwrap(),
    });

    // verify → pass
    let verdict = call(
        &mut s,
        "clawville.bounty.verify",
        json!({ "grant": grant_obj, "criteria": criteria, "expectedCriteriaHash": criteria_hash, "submission": submission }),
    )
    .await;
    assert_eq!(verdict["pass"], true, "clean submission must pass: {verdict}");
    assert_eq!(verdict["evidenceOk"], true);
    assert_eq!(verdict["scopeOk"], true);

    // release → release_payment, buyer-signed
    let decision = call(&mut s, "clawville.bounty.release", json!({ "verdict": verdict })).await;
    assert_eq!(decision["decision"], "release");
    assert_eq!(decision["instruction"], "release_payment");
    assert_eq!(decision["signerRole"], "buyer");

    // The land guard, over the same connection: grant one namespace, then
    // check that the grant opens what it names and nothing else.
    let land_grant = call(
        &mut s,
        "clawville.land.grant",
        json!({
            "parcel": "parcel-42",
            "actor": WORKER,
            "allowedActions": ["shop.*"],
            "expiresAtMs": 2_000
        }),
    )
    .await;

    let params_hash = "a".repeat(64);
    let land_action = |action: &str| {
        json!({
            "parcel": "parcel-42",
            "actor": WORKER,
            "action": action,
            "paramsHash": params_hash
        })
    };
    let authorize = |action: &str| {
        json!({ "action": land_action(action), "grant": land_grant, "nowMs": 1_000 })
    };

    let allowed = call(&mut s, "clawville.land.authorize", authorize("shop.restock")).await;
    assert_eq!(allowed["decision"], "allow");

    let refused = call(&mut s, "clawville.land.authorize", authorize("door.open")).await;
    assert_eq!(refused["decision"], "refuse");
    assert!(refused["reason"].as_str().unwrap().contains("not in the grant"));

    // Reserved beats granted: the same call with a parcel-wide grant still
    // cannot transfer the land.
    let wide = call(
        &mut s,
        "clawville.land.grant",
        json!({ "parcel": "parcel-42", "actor": WORKER, "allowedActions": ["*"] }),
    )
    .await;
    let reserved = call(
        &mut s,
        "clawville.land.authorize",
        json!({ "action": land_action("land.transfer"), "grant": wide, "nowMs": 1_000 }),
    )
    .await;
    assert_eq!(reserved["decision"], "needs_owner");
    assert_eq!(reserved["inGrant"], true, "the grant covers it; the policy is what stops it");

    let _ = child.kill().await;
}
