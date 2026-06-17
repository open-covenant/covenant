//! Live integration test: the operator capability-usage query reports accurate
//! `used`/`remaining` budget through the real daemon, and that count is read from
//! the durable ledger so it survives a restart.
//!
//! A grant carries `max_uses = 3`. After two of three units are spent through
//! real tool calls, the `CapabilityUsage` query reports `used = 2`,
//! `remaining = 1`. After the daemon is killed and respawned against the same
//! HOME the query still reports `used = 2`, `remaining = 1` — a count read from
//! an in-memory counter rather than the `uses.jsonl` ledger the enforcement path
//! folds would reset on restart and advertise a refilled `remaining = 3`, failing
//! this test. Spending the last unit drives `remaining` to 0 while the next call
//! is denied, and that denial records no further use.
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_capability_usage_introspection -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, CapabilityUsageBudget, CapabilityUsageEntry, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

fn pick_free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn read_operator_token(home: &Path) -> String {
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

async fn authenticate(stream: &mut UnixStream, home: &Path) {
    let token_b58 = read_operator_token(home).await;
    match req(stream, Request::Authenticate { token_b58 }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
}

fn spawn_daemon(home: &Path, port: u16) -> Child {
    let exe = env!("CARGO_BIN_EXE_covenantd");
    Command::new(exe)
        .env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd")
}

async fn call_echo(stream: &mut UnixStream, text: &str) -> Response {
    req(
        stream,
        Request::CallTool {
            name: "echo".into(),
            arguments: serde_json::json!({ "text": text }),
        },
    )
    .await
}

async fn usage_snapshot(stream: &mut UnixStream) -> Vec<CapabilityUsageEntry> {
    match req(stream, Request::CapabilityUsage).await {
        Response::CapabilityUsage { grants } => grants,
        other => panic!("expected CapabilityUsage, got {other:?}"),
    }
}

/// The grant identified by `signature_b58` — the ed25519 join key — or a panic
/// if the snapshot does not carry it.
fn entry_for<'a>(grants: &'a [CapabilityUsageEntry], signature_b58: &str) -> &'a CapabilityUsageEntry {
    grants
        .iter()
        .find(|g| g.signature_b58 == signature_b58)
        .unwrap_or_else(|| panic!("grant {signature_b58} must appear in the usage snapshot"))
}

fn budget_of(grants: &[CapabilityUsageEntry], signature_b58: &str) -> CapabilityUsageBudget {
    entry_for(grants, signature_b58)
        .budget
        .expect("a max_uses grant must report its budget")
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies the capability-usage query reports durable used/remaining across restart"]
async fn live_covenantd_capability_usage_query_reports_durable_remaining() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // ── Phase 1: grant max_uses = 3, spend two units, and confirm the query
    //     reports the spend (used = 2, remaining = 1) keyed by the grant's own
    //     signature.
    let grant_sig_b58;
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #1");
        authenticate(&mut stream, home.path()).await;

        grant_sig_b58 = match req(
            &mut stream,
            Request::GrantCapability {
                action: "tool.call.echo".into(),
                scope: Some(serde_json::json!({ "version": 1, "tool": "echo", "max_uses": 3 })),
                expires_at: None,
            },
        )
        .await
        {
            Response::CapabilityGranted {
                signature_b58,
                action,
                ..
            } => {
                assert_eq!(action, "tool.call.echo");
                signature_b58
            }
            other => panic!("grant tool.call.echo failed: {other:?}"),
        };

        // Before any use the budget reads full: used = 0, remaining = 3. The
        // action, perpetual expiry, and unrevoked status surface on the same row.
        let grants = usage_snapshot(&mut stream).await;
        let entry = entry_for(&grants, &grant_sig_b58);
        assert_eq!(entry.action, "tool.call.echo");
        assert_eq!(entry.expires_at, None, "the grant is perpetual");
        assert!(!entry.revoked, "the grant is live");
        assert_eq!(
            entry.budget,
            Some(CapabilityUsageBudget {
                max_uses: 3,
                used: 0,
                remaining: 3,
            }),
            "a freshly granted budget reports the full allowance",
        );

        // Spend two of three units through real tool calls.
        for i in 0..2 {
            match call_echo(&mut stream, &format!("use {i}")).await {
                Response::ToolResult { is_error, .. } => {
                    assert!(!is_error, "call {i} is within budget and must run")
                }
                other => panic!("expected a tool result within budget, got {other:?}"),
            }
        }

        // The query reflects consumed budget — not a snapshot frozen at grant
        // time: used = 2, remaining = 1.
        let grants = usage_snapshot(&mut stream).await;
        assert_eq!(
            budget_of(&grants, &grant_sig_b58),
            CapabilityUsageBudget {
                max_uses: 3,
                used: 2,
                remaining: 1,
            },
            "two of three units spent leaves one remaining",
        );

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind; remove it so wait_for_sock after
    // daemon #2 is a real readiness signal, not the stale file from #1.
    let _ = std::fs::remove_file(&sock);

    // The spent units must be on disk in uses.jsonl — the never-purged file the
    // query folds — so the count carries across the restart below.
    let uses_path = home.path().join("capabilities").join("uses.jsonl");
    let uses = std::fs::read_to_string(&uses_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", uses_path.display()));
    assert!(
        !uses.trim().is_empty(),
        "uses.jsonl must record the spent units before restart: {uses:?}"
    );

    // ── Phase 2: respawn against the same HOME. The query must still report the
    //     durable count, then the last unit spends to remaining = 0 and the next
    //     call is denied.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }
        let mut stream = UnixStream::connect(&sock).await.expect("connect #2");
        authenticate(&mut stream, home.path()).await;

        // A restart neither refills nor loses the count: used = 2, remaining = 1,
        // read from the same uses.jsonl ledger the enforcement path folds. An
        // in-memory counter would advertise a refilled remaining = 3 here.
        let grants = usage_snapshot(&mut stream).await;
        assert_eq!(
            budget_of(&grants, &grant_sig_b58),
            CapabilityUsageBudget {
                max_uses: 3,
                used: 2,
                remaining: 1,
            },
            "the restart must report the durable count, neither refilled nor lost",
        );

        // The one remaining unit still authorizes — the reported remaining = 1 is
        // the exact budget the enforcement path honors.
        match call_echo(&mut stream, "last unit").await {
            Response::ToolResult { is_error, .. } => {
                assert!(!is_error, "the last unit is within budget and must run")
            }
            other => panic!("expected the last unit to run, got {other:?}"),
        }

        let grants = usage_snapshot(&mut stream).await;
        assert_eq!(
            budget_of(&grants, &grant_sig_b58),
            CapabilityUsageBudget {
                max_uses: 3,
                used: 3,
                remaining: 0,
            },
            "spending the last unit drives remaining to 0",
        );

        // remaining = 0 is the same boundary enforcement refuses at: the next
        // call is denied.
        match call_echo(&mut stream, "over budget").await {
            Response::Error { .. } => {}
            other => panic!("a spent budget must refuse the call, got {other:?}"),
        }

        // The denied call consumes nothing: the query still reports remaining = 0,
        // not an over-count past max_uses.
        let grants = usage_snapshot(&mut stream).await;
        assert_eq!(
            budget_of(&grants, &grant_sig_b58),
            CapabilityUsageBudget {
                max_uses: 3,
                used: 3,
                remaining: 0,
            },
            "a denied call records no further use",
        );

        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
