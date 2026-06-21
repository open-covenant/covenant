//! Live HTTP coverage for the gateway bearer-auth parse pipeline
//! (`require_bearer`, covenantd/src/http.rs).
//!
//! The middleware has four distinct credential-probe rejection/acceptance
//! branches, each carrying a different operational signal:
//!   1. missing Authorization header  -> "missing Authorization header"
//!   2. header with no space separator -> "expected `Authorization: Bearer <token>`"
//!   3. scheme is not `bearer` (RFC 7235 §2.1 is case-insensitive)
//!                                    -> "expected `Authorization: Bearer <token>`"
//!   4. token is not valid base58     -> "malformed bearer token"
//! Only branch 1 has any direct coverage today (lib.rs unit sites plus the
//! bearer-gated 401 in `live_http_identity_sign.rs`). Branches 2-4 — the
//! header-format, scheme, and token-decode arms — have zero direct or
//! adjacent coverage: every existing live test sends a canonical
//! `Bearer <valid>` header, and the only 401 live assertion sends no header
//! at all. The closed-set invariant test in lib.rs pins that the six reason
//! *literals* exist in production code, but proves neither that each branch
//! *fires* over the real socket nor that they stay distinct under mutation.
//!
//! Three regressions survive every existing test:
//!   - `eq_ignore_ascii_case("bearer")` degrading to a case-sensitive compare
//!     silently rejects RFC 7235-compliant non-canonical-case schemes;
//!   - dropping the scheme gate lets `Authorization: Basic <validtoken>`
//!     authenticate (the valid bootstrap token clears decode + resolve);
//!   - collapsing the base58-decode error arm merges "malformed bearer token"
//!     into "unknown or revoked token", erasing the probe-category triage
//!     signal operators use to tell a format probe from a token-guessing probe.
//!
//! One scenario drives each branch over the gateway exclusively: spawn the
//! daemon, then GET a protected operator-readable route (`/peers/list`) with
//! five raw Authorization header values. Two non-canonical-case schemes must
//! authenticate (proving case-insensitive acceptance); a wrong scheme, a
//! header with no space, and a malformed token must each 401 with its
//! specific reason. Asserting the exact per-branch message — not just the
//! 401 status — is what pins branch-distinctness: a mutation that merges two
//! arms changes the wire message even though both stay 401.
//!
//! Hermetic — the operator token is the daemon-issued bootstrap, so no
//! operator keys, signer, or chain are needed. `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_bearer_auth_parse_rejections
//! -- --ignored live_`.

use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// The reason emitted by the no-space and wrong-scheme arms. Mirrors the
/// `&'static str` passed to `reject()` in `require_bearer` — pinning the wire
/// message in lockstep with the parse arms.
const FORMAT_REASON: &str = "expected `Authorization: Bearer <token>`";
const MALFORMED_REASON: &str = "malformed bearer token";

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

/// GET the protected `/peers/list` route with a raw Authorization header
/// value, returning (status, json body). A valid bearer yields 200
/// `{"kind":"peer_list", ...}`; any parse-rejection yields 401
/// `{"kind":"error","message": <reason>}`.
async fn probe(base: &str, auth_header_value: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("{base}/peers/list?limit=20"))
        .header(reqwest::header::AUTHORIZATION, auth_header_value)
        .send()
        .await
        .expect("send probe");
    let status = response.status();
    let body: Value = response.json().await.expect("probe body json");
    (status, body)
}

#[tokio::test]
#[ignore = "live: spawns covenantd + asserts require_bearer accepts case-insensitive bearer schemes and rejects wrong-scheme / missing-space / malformed-token with distinct per-branch 401 messages"]
async fn live_http_bearer_auth_parse_branches_distinct() {
    let home = tempfile::tempdir().expect("tempdir");
    let (mut child, base) = spawn_http_daemon(home.path()).await;
    let token = read_operator_token(home.path()).await;

    // (a)+(b) RFC 7235 §2.1: the scheme is case-insensitive. A non-canonical-
    // case scheme carrying the valid operator token MUST authenticate and
    // reach the handler. Bites `eq_ignore_ascii_case("bearer")` degrading to
    // any case-sensitive compare, which would 401 these instead.
    for scheme in ["BEARER", "bearer"] {
        let (status, body) = probe(&base, &format!("{scheme} {token}")).await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "case-insensitive scheme {scheme:?} must authenticate: {body:?}",
        );
        assert_eq!(
            body["kind"], "peer_list",
            "authenticated probe must reach /peers/list: {body:?}",
        );
    }

    // (c) Wrong scheme carrying a VALID token. The scheme gate must reject
    // before decode + resolve run — otherwise `Authorization: Basic <token>`
    // would authenticate on the valid bootstrap token (auth bypass). A probe
    // with an invalid token would still reject at decode and hide the bypass.
    let (status, body) = probe(&base, &format!("Basic {token}")).await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "wrong scheme must be rejected: {body:?}",
    );
    assert_eq!(
        body["message"], FORMAT_REASON,
        "wrong-scheme probe must report the format reason: {body:?}",
    );

    // (d) Header with no space separator. `split_once(' ')` returns None, so
    // the request must 401 before any scheme/decode check runs.
    let (status, body) = probe(&base, &format!("Bearer{token}")).await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "header with no space must be rejected: {body:?}",
    );
    assert_eq!(
        body["message"], FORMAT_REASON,
        "no-space probe must report the format reason: {body:?}",
    );

    // (e) Well-formed header, malformed (non-base58) token. Must 401 with the
    // decode-specific reason, distinct from the format arms above and from the
    // "unknown or revoked token" a collapsed-decode regression would emit.
    let (status, body) = probe(&base, "Bearer !!!invalid").await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "malformed token must be rejected: {body:?}",
    );
    assert_eq!(
        body["message"], MALFORMED_REASON,
        "malformed-token probe must report the decode reason: {body:?}",
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}
