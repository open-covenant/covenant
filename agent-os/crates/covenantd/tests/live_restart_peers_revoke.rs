//! Live integration test: peers-revoke survives daemon restart.
//!
//! Closes Sprint 68's "crash-mid-revoke" expected production failure
//! mode. Spawns covenantd against a tempdir HOME pre-seeded with a
//! guest peer, has the operator revoke the guest, then SIGKILLs the
//! daemon, respawns against the same HOME, and verifies that
//!
//! 1. the on-disk `peers/registry.jsonl` carries the `revoked` event
//!    (the `JsonlPeerRegistry`'s persist-then-mutate ordering means
//!    this is the only way the revoke could have made it past the
//!    IPC reply we observed before the kill);
//! 2. the revoked guest token is rejected on a fresh authenticate
//!    against the new daemon (replay-on-`open` correctly rebuilt the
//!    `revoked` map);
//! 3. the operator token still authenticates (the revocation is
//!    scoped to the guest's token-prefix, not a global reset that
//!    `bootstrap_operator_token` would have to re-mint).
//!
//! Mirrors `live_restart_a2a`'s two-phase shape (Sprint 44) and
//! `live_peers_revoke`'s pre-seed flow (Sprint 68).
//!
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_restart_peers_revoke -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
use covenant_peer_auth::{JsonlPeerRegistry, PeerEntry, PeerRegistry, PeerToken, RevokeOutcome};
use covenant_types::AgentId;
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

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticate(stream: &mut UnixStream, token_b58: &str) -> Response {
    req(
        stream,
        Request::Authenticate {
            token_b58: token_b58.to_string(),
        },
    )
    .await
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

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies JsonlPeerRegistry revoke replay across restart"]
async fn live_covenantd_peers_revoke_survives_daemon_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");

    // Pre-seed `peers/registry.jsonl` with a guest before either
    // daemon starts. Same shape as Sprint 68's live_peers_revoke;
    // the daemon's own `JsonlPeerRegistry::open()` replays this on
    // boot, then `bootstrap_operator_token` appends the operator
    // entry alongside.
    let guest_token = PeerToken::generate();
    let guest_token_b58 = guest_token.to_b58();
    let guest_pubkey = [42u8; 32];
    let guest_display = "guest@local";
    let guest_entry = PeerEntry {
        token: guest_token,
        agent_id: AgentId::new(guest_display, guest_pubkey),
        registered_at: 1_700_000_000_000,
    };
    let registry_path = home.path().join("peers").join("registry.jsonl");
    {
        let registry = JsonlPeerRegistry::open(registry_path.clone())
            .await
            .expect("open seed registry");
        registry.register(guest_entry).await.expect("seed guest");
    }

    // ── Phase 1: daemon #1 — operator revokes the guest, then we
    //     SIGKILL so the test exercises the on-disk replay path
    //     rather than any clean-shutdown drain logic.
    let operator_token: String;
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }

        operator_token = read_operator_token(home.path()).await;
        assert_ne!(
            operator_token, guest_token_b58,
            "operator and guest tokens must differ"
        );

        // Sanity: the pre-seeded guest authenticates pre-revoke.
        // A regression here would make the post-restart rejection
        // assertion vacuous.
        {
            let mut stream = UnixStream::connect(&sock).await.expect("connect guest");
            match authenticate(&mut stream, &guest_token_b58).await {
                Response::Authenticated { display } => {
                    assert_eq!(display, guest_display, "guest display mismatch");
                }
                other => panic!("guest auth pre-revoke must succeed, got {other:?}"),
            }
        }

        // Operator authenticates and revokes the guest by full b58
        // prefix (collision with the daemon-minted operator token's
        // prefix is 1/58^44, effectively zero — same argument
        // Sprint 68 makes).
        {
            let mut stream = UnixStream::connect(&sock).await.expect("connect operator");
            match authenticate(&mut stream, &operator_token).await {
                Response::Authenticated { .. } => {}
                other => panic!("operator auth failed: {other:?}"),
            }
            let resp = req(
                &mut stream,
                Request::RevokePeer {
                    token_prefix: guest_token_b58.clone(),
                    force: false,
                },
            )
            .await;
            match resp {
                Response::PeerRevoked {
                    outcome: RevokeOutcome::Revoked(summary),
                } => {
                    assert_eq!(summary.agent_id.display, guest_display);
                    assert_eq!(summary.agent_id.pubkey, guest_pubkey);
                    assert!(
                        summary.revoked_at.is_some(),
                        "revoked_at must be set on success"
                    );
                }
                other => panic!("expected RevokeOutcome::Revoked, got {other:?}"),
            }
        }

        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the socket file behind. Remove it so
    // `wait_for_sock` after daemon #2 spawn observes the new
    // listener, not the stale node from #1.
    let _ = std::fs::remove_file(&sock);

    // The on-disk JSONL must carry the revoke event before respawn —
    // if the persist hadn't completed, the IPC `Revoked` response
    // we observed above would have been a lie about durable state.
    // `JsonlPeerRegistry::revoke_by_token_prefix` is persist-then-
    // mutate, so the response implies the line is on disk.
    let registry_text = std::fs::read_to_string(&registry_path).expect("read registry.jsonl");
    assert!(
        registry_text.contains("\"type\":\"revoked\""),
        "registry.jsonl missing revoked line before respawn: {registry_text}"
    );

    // ── Phase 2: daemon #2 — same HOME, different port. Replay-on-
    //     open must rebuild the revoked map from the JSONL we just
    //     verified, so the guest token stays rejected.
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }

        // Replay assertion #1: guest auth fails. Reason names the
        // token state and the daemon closes the connection (matches
        // Sprint 47's `live_covenantd_rejects_unauthenticated_connection`).
        {
            let mut stream = UnixStream::connect(&sock).await.expect("connect guest #2");
            match authenticate(&mut stream, &guest_token_b58).await {
                Response::AuthenticationFailed { reason } => {
                    assert!(
                        reason.contains("unknown") || reason.contains("revoked"),
                        "rejection must name token state; got {reason:?}"
                    );
                }
                other => panic!("expected AuthenticationFailed post-restart, got {other:?}"),
            }
            let next = read_frame::<_, Response>(&mut stream).await;
            match next {
                Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
                other => panic!("expected EOF after auth failure, got {other:?}"),
            }
        }

        // Replay assertion #2: operator auth still works. Confirms
        // (a) `bootstrap_operator_token` is idempotent across
        // restarts (reads existing token + sees its registry entry
        // already in the JSONL replay), and (b) the revocation was
        // scoped to the guest's prefix, not a global reset.
        {
            let mut stream = UnixStream::connect(&sock)
                .await
                .expect("connect operator #2");
            match authenticate(&mut stream, &operator_token).await {
                Response::Authenticated { .. } => {}
                other => panic!("operator auth post-restart must succeed, got {other:?}"),
            }
            match req(&mut stream, Request::Ping).await {
                Response::Pong => {}
                other => panic!("post-restart operator ping failed: {other:?}"),
            }
        }

        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
