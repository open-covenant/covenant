//! Live integration test: spawns covenantd against a tempdir HOME and drives
//! `Request::SignAttestation` over the raw IPC socket — pinning the success
//! `Response::IdentityAttestation { signature_b58, pubkey_b58, ts }` receipt
//! and the four rejection paths (missing capability, stale timestamp, empty
//! and oversized message).
//!
//! `sign_attestation` (covenantd/src/lib.rs:4677) gates on the `identity.attest`
//! capability, enforces a 120s timestamp-freshness window, bounds the bs58
//! message to 1..=4096 bytes, then signs `b"covenant.identity.attest.v1\n"`
//! concatenated with the message using the daemon's own ed25519 identity. The
//! verb has in-lib unit coverage but is never exercised over the Unix socket
//! the CLI is built on; this pins that wire contract.
//!
//! The happy path is hermetic: the daemon self-signs with `self.identity`, not
//! the SAP bridge, so no operator keys, signer, or chain are needed. The
//! receipt is pinned byte-exact by loading the daemon identity from
//! `home/identity/local.key` (the same derivation `live_a2a.rs` uses) and
//! re-signing the domain-separated preimage — RFC 8032 ed25519 is
//! deterministic, so a dropped or altered domain prefix, or a wrong signing
//! key, fails the equality even though the response variant would still look
//! well-formed. Hermetic — no external services. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_ipc_sign_attestation -- --ignored live_`.

use covenant_identity::LocalIdentity;
use covenant_ipc::{read_frame, write_frame, Request, Response};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// Domain separation prefix the daemon signs under — mirror of the private
/// `ATTEST_DOMAIN` const in `sign_attestation`. The byte-exact signature
/// assertion below is what pins these two in lockstep.
const ATTEST_DOMAIN: &[u8] = b"covenant.identity.attest.v1\n";

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
    child
}

async fn req(stream: &mut UnixStream, request: Request) -> Response {
    write_frame(stream, &request).await.expect("write_frame");
    read_frame(stream).await.expect("read_frame")
}

async fn authenticated_stream(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("sock"))
        .await
        .expect("connect socket");
    let token = read_operator_token(home).await;
    match req(&mut stream, Request::Authenticate { token_b58: token }).await {
        Response::Authenticated { .. } => {}
        other => panic!("authenticate failed: {other:?}"),
    }
    stream
}

/// Self-grant `identity.attest` to the authenticated operator. `check_capabilities`
/// has no operator bypass, so the grant is a precondition of every success path.
async fn grant_identity_attest(stream: &mut UnixStream) {
    match req(
        stream,
        Request::GrantCapability {
            action: "identity.attest".into(),
            scope: None,
            expires_at: None,
        },
    )
    .await
    {
        Response::CapabilityGranted { .. } => {}
        other => panic!("expected Response::CapabilityGranted, got {other:?}"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as u64
}

/// The daemon's on-disk ed25519 identity — the same key `sign_attestation`
/// signs with. `live_a2a.rs` derives the operator pubkey the same way.
fn daemon_identity(home: &Path) -> LocalIdentity {
    let path = home.join("identity").join("local.key");
    LocalIdentity::load_or_create(&path, "user@local").expect("load daemon identity")
}

#[tokio::test]
#[ignore = "live: spawns covenantd + drives Request::SignAttestation over the socket, pinning Response::IdentityAttestation against a deterministic re-sign of the daemon identity"]
async fn live_ipc_sign_attestation_returns_verifiable_receipt() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    grant_identity_attest(&mut stream).await;

    let message = b"covenant identity attestation live test";
    let message_b58 = bs58::encode(message).into_string();
    let ts = now_ms();

    match req(&mut stream, Request::SignAttestation { message_b58, ts }).await {
        Response::IdentityAttestation {
            signature_b58,
            pubkey_b58,
            ts: echoed_ts,
        } => {
            let id = daemon_identity(home.path());

            // The attestation must be signed by the daemon's own identity —
            // sign_attestation uses self.identity, never a per-peer or bridge key.
            assert_eq!(
                pubkey_b58,
                bs58::encode(id.pubkey_bytes()).into_string(),
                "attestation pubkey must be the daemon identity pubkey"
            );

            // Byte-exact signature over the domain-separated preimage. ed25519
            // (RFC 8032) is deterministic, so a dropped or altered ATTEST_DOMAIN
            // prefix, or a different signing key, fails this equality even though
            // Response::IdentityAttestation would still look well-formed.
            let mut signed = ATTEST_DOMAIN.to_vec();
            signed.extend_from_slice(message);
            assert_eq!(
                signature_b58,
                bs58::encode(id.sign(&signed).to_bytes()).into_string(),
                "signature must cover exactly b\"covenant.identity.attest.v1\\n\" || message"
            );

            assert_eq!(echoed_ts, ts, "receipt must echo the request timestamp");
        }
        other => panic!("expected Response::IdentityAttestation, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts Request::SignAttestation without the identity.attest capability flattens onto Response::Error"]
async fn live_ipc_sign_attestation_requires_capability() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;

    // No grant: the authenticated operator still lacks identity.attest because
    // check_capabilities has no operator bypass.
    match req(
        &mut stream,
        Request::SignAttestation {
            message_b58: bs58::encode(b"unauthorized").into_string(),
            ts: now_ms(),
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains("identity.attest"),
                "rejection must name the missing capability, got: {message}"
            );
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts a stale Request::SignAttestation timestamp flattens onto Response::Error"]
async fn live_ipc_sign_attestation_rejects_stale_timestamp() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    grant_identity_attest(&mut stream).await;

    // ts = 1 is ~now milliseconds in the past — far outside the 120_000ms window.
    match req(
        &mut stream,
        Request::SignAttestation {
            message_b58: bs58::encode(b"stale request").into_string(),
            ts: 1,
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains("freshness window"),
                "stale timestamp must be rejected by the freshness gate, got: {message}"
            );
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts an over-length Request::SignAttestation message flattens onto Response::Error"]
async fn live_ipc_sign_attestation_rejects_oversized_message() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    grant_identity_attest(&mut stream).await;

    // 5000 bytes decodes above the 1..=4096 upper bound the daemon enforces
    // before it ever reaches the signing key.
    let oversized = bs58::encode(vec![7u8; 5000]).into_string();
    match req(
        &mut stream,
        Request::SignAttestation {
            message_b58: oversized,
            ts: now_ms(),
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains("1..=4096 bytes"),
                "over-length message must be rejected by the size bound, got: {message}"
            );
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts an empty Request::SignAttestation message flattens onto Response::Error"]
async fn live_ipc_sign_attestation_rejects_empty_message() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = spawn_daemon(home.path()).await;
    let mut stream = authenticated_stream(home.path()).await;
    grant_identity_attest(&mut stream).await;

    // An empty payload decodes to 0 bytes — below the 1..=4096 lower bound, so
    // the daemon must refuse rather than sign the bare ATTEST_DOMAIN prefix with
    // no message. Guards the opposite end of the bound from the oversized test.
    let empty = bs58::encode(b"").into_string();
    match req(
        &mut stream,
        Request::SignAttestation {
            message_b58: empty,
            ts: now_ms(),
        },
    )
    .await
    {
        Response::Error { message } => {
            assert!(
                message.contains("1..=4096 bytes"),
                "empty message must be rejected by the size bound, got: {message}"
            );
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }

    drop(stream);
    let _ = child.kill().await;
    let _ = child.wait().await;
}
