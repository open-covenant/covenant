//! Live HTTP coverage for `POST /identity/sign` (`identity_sign`).
//!
//! Daemon attestation signing ships and is daemon-tested over the Unix socket
//! (`live_ipc_sign_attestation.rs`), but no test drives it over the HTTP
//! gateway. The route is a thin handler that deserializes `{message_b58, ts}`
//! into `Request::SignAttestation` and forwards it to `Server::respond`, which
//! signs with the daemon's own ed25519 identity. That leaves the gateway's
//! deserialize-and-forward path and the signature it returns unproven on HTTP:
//! a handler that dropped the `ts` binding, signed the wrong bytes, or returned
//! a malformed signature would still pass a test that only checked the response
//! kind.
//!
//! `sign_attestation` (covenantd/src/lib.rs) gates on the `identity.attest`
//! capability, enforces a timestamp-freshness window, bounds the bs58 message,
//! then signs `b"covenant.identity.attest.v1\n"` concatenated with the decoded
//! message using `self.identity`, returning `Response::IdentityAttestation
//! { signature_b58, pubkey_b58, ts }` (`kind: "identity_attestation"`).
//!
//! One scenario over the gateway exclusively: authenticate as the operator,
//! confirm `POST /identity/sign` is rejected `401` without a bearer (it sits in
//! the protected router), grant `identity.attest`, then sign a fixed message
//! and verify the receipt cryptographically rather than by shape. The pubkey
//! must be the daemon's own identity key, and — because RFC 8032 ed25519 is
//! deterministic — the signature must be byte-identical to a local re-sign of
//! the domain-separated preimage. A dropped or altered domain prefix, a wrong
//! signing key, or a dropped `ts` echo fails the equality even though the
//! `IdentityAttestation` envelope would still look well-formed.
//!
//! Hermetic — the daemon self-signs, so no operator keys, signer, or chain are
//! needed. `#[ignore]`'d. Each test uses its own tempdir to stay safe under
//! `--test-threads`. Run with
//! `cargo test -p covenantd --test live_http_identity_sign -- --ignored live_`.

use covenant_identity::LocalIdentity;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// Domain separation prefix the daemon signs under — mirror of the private
/// `ATTEST_DOMAIN` const in `sign_attestation`. The byte-exact signature
/// assertion below is what pins these two in lockstep.
const ATTEST_DOMAIN: &[u8] = b"covenant.identity.attest.v1\n";

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
        if let Ok(text) = std::fs::read_to_string(&path) {
            let token = text.trim();
            if !token.is_empty() {
                return token.to_string();
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("operator token never appeared at {}", path.display());
}

async fn wait_for_http(base: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    for _ in 0..80 {
        match client.get(format!("{base}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            _ => sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("http gateway never became healthy at {base}/health");
}

async fn spawn_http_daemon(home: &Path) -> (Child, String) {
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
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
    wait_for_http(&base).await;
    (child, base)
}

async fn operator_client(home: &Path) -> reqwest::Client {
    let token = read_operator_token(home).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client")
}

/// The daemon's on-disk ed25519 identity — the same key `sign_attestation`
/// signs with. `live_ipc_sign_attestation.rs` derives it the same way.
fn daemon_identity(home: &Path) -> LocalIdentity {
    LocalIdentity::load_or_create(&home.join("identity").join("local.key"), "user@local")
        .expect("load daemon identity")
}

async fn grant(client: &reqwest::Client, base: &str, action: &str) {
    let granted: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": action }))
        .send()
        .await
        .expect("send capability grant")
        .json()
        .await
        .expect("grant body json");
    assert_eq!(
        granted["kind"], "capability_granted",
        "grant {action} must succeed: {granted:?}",
    );
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as u64
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts POST /identity/sign is bearer-gated and returns a daemon signature that verifies byte-exact over HTTP"]
async fn live_http_identity_sign_returns_verifiable_signature_over_http() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let client = operator_client(home.path()).await;

    let message = b"covenant identity attestation http live test";
    let message_b58 = bs58::encode(message).into_string();
    let ts = now_ms();

    // Bearer gate: the route sits in the protected router, so an unauthenticated
    // POST must be rejected before the handler runs — a regression that drops
    // /identity/sign out of the protected layer (signing on demand for anyone)
    // would surface here.
    let unauth = reqwest::Client::new()
        .post(format!("{base}/identity/sign"))
        .json(&json!({ "message_b58": message_b58, "ts": ts }))
        .send()
        .await
        .expect("send unauthenticated sign request");
    assert_eq!(
        unauth.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "POST /identity/sign without a bearer must be 401",
    );

    // No operator bypass on capabilities: the grant is a precondition of the
    // success path even for the operator bearer.
    grant(&client, &base, "identity.attest").await;

    let receipt: Value = client
        .post(format!("{base}/identity/sign"))
        .json(&json!({ "message_b58": message_b58, "ts": ts }))
        .send()
        .await
        .expect("post identity sign")
        .json()
        .await
        .expect("attestation body json");
    assert_eq!(
        receipt["kind"], "identity_attestation",
        "sign must return an identity_attestation envelope: {receipt:?}",
    );

    let signature_b58 = receipt["signature_b58"]
        .as_str()
        .expect("signature_b58 string");
    let pubkey_b58 = receipt["pubkey_b58"].as_str().expect("pubkey_b58 string");
    let echoed_ts = receipt["ts"].as_u64().expect("ts u64");

    let id = daemon_identity(home.path());

    // The attestation must be signed by the daemon's own identity —
    // sign_attestation uses self.identity, never a per-peer or bridge key.
    assert_eq!(
        pubkey_b58,
        bs58::encode(id.pubkey_bytes()).into_string(),
        "attestation pubkey must be the daemon identity pubkey: {receipt:?}",
    );

    // Byte-exact signature over the domain-separated preimage. ed25519
    // (RFC 8032) is deterministic, so a dropped or altered ATTEST_DOMAIN prefix,
    // or a different signing key, fails this equality even though the envelope
    // would still look well-formed.
    let mut signed = ATTEST_DOMAIN.to_vec();
    signed.extend_from_slice(message);
    assert_eq!(
        signature_b58,
        bs58::encode(id.sign(&signed).to_bytes()).into_string(),
        "signature must cover exactly b\"covenant.identity.attest.v1\\n\" || message over HTTP",
    );

    assert_eq!(echoed_ts, ts, "receipt must echo the request timestamp");

    let _ = child.kill().await;
    let _ = child.wait().await;
}
