//! Live integration test: `--force` deliberately bypasses the peer
//! self-revoke guard, tombstones the operator's own live token (bricking
//! auth), and the documented recovery is manual — delete the operator
//! token file so `bootstrap_operator_token` regenerates a fresh token on
//! the next start. The revoked token is never auto-resurrected.
//!
//! `revoke_peer` guards against an operator fat-fingering their own
//! token: without `--force`, a revoke whose unique live match is the
//! caller's own identity returns `SelfRevokeForbidden` and mutates
//! nothing (covered by `live_cli_peers_self_revoke.rs`). With
//! `force == true` the daemon skips that guard (lib.rs `if !force`) and
//! proceeds straight to `revoke_by_token_prefix`, tombstoning the
//! operator's bootstrap credential. That bricks every subsequent
//! authenticate on the running daemon — `resolve` checks the in-memory
//! `revoked` map first and returns `None`.
//!
//! Recovery is deliberately not automatic. `bootstrap_operator_token`
//! reads `$COVENANT_HOME/peers/operator.token`; when the file is absent
//! it mints a fresh token, but when the file is present and parseable it
//! reuses that exact token (even if the registry has it revoked —
//! re-registering a tombstoned token does not clear the `revoked` map,
//! and `resolve` still returns `None`). So the only way back from a
//! forced self-revoke is operator action: stop the daemon, remove the
//! token file, and restart, which regenerates a brand-new credential.
//! This is the safe posture — a revoked credential is never silently
//! resurrected, which would defeat revoking a compromised token.
//!
//! This drives the full sequence through real daemon processes and the
//! real on-disk `JsonlPeerRegistry`: Phase 1 force-revokes the
//! operator's own token over IPC and proves auth with it now fails on a
//! fresh connection; Phase 2 deletes the token file, respawns against
//! the same HOME, and proves a different token was minted (recovery)
//! while the old revoked token stays rejected across the restart. The
//! non-force refusal is already pinned by `live_cli_peers_self_revoke`;
//! the `force` bypass and the manual-recovery shape are the uncovered
//! edges here.
//!
//! Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_peers_self_revoke_force_recovery -- --ignored live_`.

use covenant_ipc::{read_frame, write_frame, IpcError, Request, Response};
use covenant_peer_auth::RevokeOutcome;
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

/// Authenticate and assert the connection is refused because the token
/// is revoked. The daemon closes the socket after an auth failure, so the
/// following read surfaces EOF — pinning both the rejection and that the
/// failed handshake tears the connection down.
async fn assert_revoked_rejected(sock: &Path, token_b58: &str) {
    let mut stream = UnixStream::connect(sock).await.expect("connect revoked token");
    match authenticate(&mut stream, token_b58).await {
        Response::AuthenticationFailed { reason } => assert!(
            reason.contains("unknown") || reason.contains("revoked"),
            "a revoked operator token must be rejected, got reason: {reason:?}",
        ),
        Response::Authenticated { .. } => {
            panic!("revoked operator token must NOT authenticate, but was admitted")
        }
        other => panic!("expected AuthenticationFailed for revoked token, got {other:?}"),
    }
    let next = read_frame::<_, Response>(&mut stream).await;
    match next {
        Err(IpcError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        other => panic!("expected EOF after revoked-token auth failure, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "live: spawns covenantd twice + verifies forced operator self-revoke bricks auth and manual token-file removal recovers via regeneration"]
async fn live_covenantd_forced_self_revoke_bricks_and_manual_recovery_regenerates() {
    let home = tempfile::tempdir().expect("tempdir");
    let sock = home.path().join("sock");
    let token_path = home.path().join("peers").join("operator.token");

    // ── Phase 1: daemon #1 — the operator force-revokes their own
    //     bootstrap token. `force == true` skips the self-revoke guard
    //     and tombstones the operator's live entry; a fresh connection
    //     authenticating with that same token is then refused.
    let bricked_token: String;
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #1 never created its socket at {}", sock.display());
        }

        bricked_token = read_operator_token(home.path()).await;

        // Sanity: the bootstrap token authenticates before the revoke.
        // Without this, the post-revoke rejection assertion is vacuous.
        {
            let mut stream = UnixStream::connect(&sock).await.expect("connect operator pre-revoke");
            match authenticate(&mut stream, &bricked_token).await {
                Response::Authenticated { .. } => {}
                other => panic!("operator auth must succeed pre-revoke, got {other:?}"),
            }
        }

        // The operator authenticates and force-revokes their own token by
        // its full base58 prefix. force must bypass the self-revoke guard
        // and reach `revoke_by_token_prefix`, returning Revoked.
        {
            let mut stream = UnixStream::connect(&sock).await.expect("connect operator revoke");
            match authenticate(&mut stream, &bricked_token).await {
                Response::Authenticated { .. } => {}
                other => panic!("operator auth for revoke failed: {other:?}"),
            }
            match req(
                &mut stream,
                Request::RevokePeer {
                    token_prefix: bricked_token.clone(),
                    force: true,
                    match_limit: None,
                },
            )
            .await
            {
                Response::PeerRevoked {
                    outcome: RevokeOutcome::Revoked(summary),
                } => {
                    assert!(
                        summary.revoked_at.is_some(),
                        "forced self-revoke must tombstone the operator's own token"
                    );
                }
                Response::PeerRevoked {
                    outcome: RevokeOutcome::SelfRevokeForbidden(_),
                } => panic!(
                    "force=true must bypass the self-revoke guard, but the daemon returned \
                     SelfRevokeForbidden (the guard ran anyway)"
                ),
                other => panic!("expected Revoked from forced self-revoke, got {other:?}"),
            }
        }

        // The brick: a fresh connection authenticating with the now-revoked
        // operator token is refused. resolve() sees the token in the
        // revoked map and returns None before the registry entry is read.
        assert_revoked_rejected(&sock, &bricked_token).await;

        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // SIGKILL leaves the stale socket; remove it so the respawn wait
    // observes daemon #2's listener, not #1's dead node.
    let _ = std::fs::remove_file(&sock);

    // The revocation must be durable before respawn: if the revoke line
    // never reached `peers/registry.jsonl`, the IPC Revoked response we
    // saw would have lied about on-disk state.
    let registry_path = home.path().join("peers").join("registry.jsonl");
    let registry_text =
        std::fs::read_to_string(&registry_path).expect("read registry.jsonl after revoke");
    assert!(
        registry_text.contains("\"type\":\"revoked\""),
        "registry.jsonl missing revoked line after forced self-revoke: {registry_text}"
    );

    // ── Phase 2: recovery is manual. The operator stops the daemon,
    //     removes the bricked token file, and restarts.
    //     `bootstrap_operator_token` sees no file and mints a fresh token.
    //     The old token stays revoked across the restart — it is never
    //     resurrected — which is what makes revoking a compromised token
    //     stick.
    std::fs::remove_file(&token_path).expect("remove bricked operator.token for recovery");

    let recovered_token: String;
    {
        let mut child = spawn_daemon(home.path(), pick_free_port());
        if !wait_for_sock(&sock).await {
            let _ = child.kill().await;
            panic!("daemon #2 never created its socket at {}", sock.display());
        }

        recovered_token = read_operator_token(home.path()).await;
        assert_ne!(
            recovered_token, bricked_token,
            "recovery must regenerate a fresh operator token, not reuse the revoked one"
        );

        // The fresh token authenticates and the session is usable —
        // recovery restored operator access.
        {
            let mut stream = UnixStream::connect(&sock)
                .await
                .expect("connect recovered operator");
            match authenticate(&mut stream, &recovered_token).await {
                Response::Authenticated { .. } => {}
                other => panic!("recovered operator token must authenticate, got {other:?}"),
            }
            match req(&mut stream, Request::Ping).await {
                Response::Pong => {}
                other => panic!("recovered operator ping failed: {other:?}"),
            }
        }

        // The bricked token is still rejected after restart: the on-disk
        // revoked event replayed into the registry's revoked map, and the
        // regenerated token is a different credential. A regression that
        // resurrected the old token (or dropped the revocation on
        // restart) surfaces here.
        assert_revoked_rejected(&sock, &bricked_token).await;

        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
