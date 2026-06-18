//! Live HTTP coverage for `POST /x402/pay` — the daemon's sole
//! outbound-spend trigger — proving it fails closed over the gateway.
//!
//! `pay_x402_route` (covenantd/src/http.rs) deserializes a `PayX402Body`
//! and forwards `Request::PayX402` to the same `Server::respond` the unix
//! socket uses. The handler gates in strict order (covenant lib.rs): the
//! `x402.outbound.pay` capability, a scope-allowed provider, a
//! configured+enabled dispatch, a valid method, and only then
//! `per_call_cap.parse::<u128>()`, which rejects a malformed cap *before*
//! any signer subprocess is constructed or network call is made. A
//! daemon-level rejection surfaces over HTTP as a 200 response whose body
//! is `{"kind":"error", ...}` — not a 4xx — so the assertions read the
//! JSON envelope, not the status line.
//!
//! This mirrors `live_ipc_pay_x402_invalid_cap.rs` (which pins the same
//! fail-closed path over the socket) across the HTTP boundary: it proves
//! the route's body deserialization, the capability gate, and the
//! cap-parse rejection end to end over the gateway. The two-step gate
//! proof — an ungranted call rejected by the capability gate, and only
//! after the grant the cap-parse becoming the rejecting gate — is what
//! pins the ordering rather than a single after-the-fact assertion.
//!
//! Hermetic — x402 dispatch is enabled from env alone (`COVENANT_X402_ENABLED`
//! + a `/bin/true` signer so `x402_dispatch_config_from_env` resolves
//! `Some`); the malformed-cap path short-circuits before the signer is
//! constructed, so no real signer, Solana RPC, or x402 endpoint is touched.
//! `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_http_x402_pay_fail_closed -- --ignored live_`.

use serde_json::{json, Value};
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

struct Daemon {
    child: Child,
    base: String,
    token: String,
    _home: tempfile::TempDir,
}

/// Spawn covenantd with x402 dispatch enabled from env. Inherited
/// `COVENANT_X402_*` are cleared first so the dispatch config is
/// host-independent; only `ENABLED` + a `/bin/true` signer are set, which
/// is enough for the config gate to resolve `Some` while the malformed-cap
/// path short-circuits before the signer is ever constructed.
async fn spawn_daemon_with_x402() -> Daemon {
    let home = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    let exe = env!("CARGO_BIN_EXE_covenantd");

    let child = Command::new(exe)
        .env("COVENANT_HOME", home.path())
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home.path())
        .env_remove("COVENANT_X402_ENABLED")
        .env_remove("COVENANT_X402_SIGNER_BINARY")
        .env_remove("COVENANT_X402_FUNDING_KEYPAIR")
        .env_remove("COVENANT_X402_RPC_URL")
        .env("COVENANT_X402_ENABLED", "1")
        .env("COVENANT_X402_SIGNER_BINARY", "/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn covenantd");

    let sock = home.path().join("sock");
    if !wait_for_sock(&sock).await {
        panic!("daemon never created its socket at {}", sock.display());
    }
    wait_for_http(&base).await;
    let token = read_operator_token(home.path()).await;

    Daemon {
        child,
        base,
        token,
        _home: home,
    }
}

impl Daemon {
    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

/// A `PayX402Body` whose only invalid field is `per_call_cap`: a valid
/// method and well-formed strings everywhere else, so once the capability,
/// scope, config, and method gates pass the cap-parse is the only gate
/// that can reject it.
fn malformed_pay_body(per_call_cap: &str) -> Value {
    json!({
        "provider": "x",
        "endpoint": "https://example.test/e",
        "method": "POST",
        "network": "solana:mainnet",
        "asset": "usdc-sol",
        "per_call_cap": per_call_cap,
        "credits": 8,
    })
}

#[tokio::test]
#[ignore = "live: spawns covenantd with x402 enabled + drives POST /x402/pay over HTTP, pinning the x402.outbound.pay capability gate then the per_call_cap u128 parse rejection over the gateway"]
async fn live_http_x402_pay_rejects_ungranted_then_malformed_cap() {
    let daemon = spawn_daemon_with_x402().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");

    let pay = |cap: &str| {
        client
            .post(format!("{}/x402/pay", daemon.base))
            .bearer_auth(&daemon.token)
            .json(&malformed_pay_body(cap))
    };

    // Step 1 — ungranted baseline. The operator clears the bearer-auth
    // identity gate, but spending still requires x402.outbound.pay, so the
    // gateway must reject on the capability gate before it ever parses the
    // cap. A daemon Error comes back as HTTP 200 with kind==error.
    let ungranted = pay("not_a_number")
        .send()
        .await
        .expect("send ungranted /x402/pay");
    assert!(
        ungranted.status().is_success(),
        "the gateway returns daemon errors as HTTP 200 with a kind==error body, not a 4xx; got {}",
        ungranted.status(),
    );
    let ungranted: Value = ungranted.json().await.expect("ungranted body json");
    assert_eq!(
        ungranted["kind"], "error",
        "an ungranted PayX402 must come back as a kind==error envelope: {ungranted:?}",
    );
    assert!(
        ungranted["message"]
            .as_str()
            .is_some_and(|m| m.contains("x402.outbound.pay")),
        "an ungranted call must be rejected by the capability gate naming x402.outbound.pay, not \
         the cap parse: {ungranted:?}",
    );

    // Step 2 — grant the spend capability to the operator peer (unscoped, so
    // the scope gate admits any provider).
    let grant: Value = client
        .post(format!("{}/capabilities/grant", daemon.base))
        .bearer_auth(&daemon.token)
        .json(&json!({ "action": "x402.outbound.pay" }))
        .send()
        .await
        .expect("grant request")
        .json()
        .await
        .expect("grant json");
    assert_eq!(
        grant["kind"], "capability_granted",
        "operator self-grant of x402.outbound.pay must succeed: {grant:?}",
    );

    // Step 3 — capability, scope, config, and method gates now all clear, so
    // the per_call_cap parse is the rejecting gate. It must fail closed (no
    // signer spawn, no network) with the cap-specific message echoing the
    // bad value.
    let malformed: Value = pay("not_a_number")
        .send()
        .await
        .expect("send malformed-cap /x402/pay")
        .json()
        .await
        .expect("malformed body json");
    assert_eq!(
        malformed["kind"], "error",
        "a malformed cap must come back as a kind==error envelope: {malformed:?}",
    );
    let message = malformed["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("invalid per_call_cap (must be decimal u128)"),
        "after the grant the malformed cap must be rejected by the u128 parse gate over HTTP: \
         {malformed:?}",
    );
    assert!(
        message.contains("not_a_number"),
        "the parse error must echo the offending cap value: {malformed:?}",
    );

    // Step 4 — a value past u128::MAX proves the gate is a real numeric parse,
    // not an is-empty / is-ascii-digit check a 40-digit string would slip past.
    let overflow = "9".repeat(40);
    let overflow_resp: Value = pay(&overflow)
        .send()
        .await
        .expect("send overflow-cap /x402/pay")
        .json()
        .await
        .expect("overflow body json");
    assert_eq!(
        overflow_resp["kind"], "error",
        "an overflow cap must come back as a kind==error envelope: {overflow_resp:?}",
    );
    assert!(
        overflow_resp["message"]
            .as_str()
            .is_some_and(|m| m.contains("invalid per_call_cap (must be decimal u128)")),
        "a cap past u128::MAX must fail the same parse gate, not be admitted: {overflow_resp:?}",
    );

    daemon.shutdown().await;
}
