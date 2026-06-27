//! End-to-end cross-host A2A delivery over two real daemons (multi-host slice 4b-4).
//!
//! Slices 4b-1/4b-2/4b-3 are unit-tested in isolation: the signed envelope, the
//! receiver-side admission pipeline, and the sender-side delivery + routing each
//! have in-process coverage. None of them prove the seam survives two real
//! `covenantd` processes talking over a real HTTP socket. This test spins up two
//! daemons on loopback, cross-registers them in each other's known-hosts
//! registry, and proves an authenticated A2A task sent on daemon A is delivered
//! to daemon B's mailbox — and never enqueued on A's own.
//!
//! Provisioning is deterministic. Each daemon's known-hosts file is read once at
//! startup, so both pubkeys must be known before either process spawns: the test
//! pre-creates both identity key files (the same `load_or_create` the daemon
//! runs, so each daemon re-loads the exact key) to learn both pubkeys, then
//! serializes a `KnownHosts` value to each home's `peers/known-hosts.json`. The
//! daemon's identity display is the hardcoded `user@local` (host `local`) — it is
//! not stored in the 32-byte key file and cannot be overridden — and the mailbox
//! leases tasks by full `AgentId` equality, so the cross-host recipient is
//! addressed as the remote's own `user@local` identity and each daemon registers
//! the other under host `local`. The pubkey binding, not the host string, is the
//! real authorization: admission verifies the sender's signing key equals the
//! key the registry binds to its host, and delivery verifies the recipient's key
//! equals the key bound to the destination host.
//!
//! A second test proves a configured-but-unresponsive remote cannot stall the
//! daemon: a black-hole socket that completes the TCP handshake but never answers
//! forces the outbound POST to hang until the (overridden, short) delivery
//! timeout fires, and the send returns a bounded `Response::Error` while the
//! daemon stays responsive.
//!
//! Hermetic, loopback-only, `#[ignore]`'d. Run with
//! `cargo test -p covenantd --test live_a2a_cross_host -- --ignored --nocapture`.

use covenant_a2a::A2ATask;
use covenant_types::AgentId;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::time::sleep;
use uuid::Uuid;

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

/// Load-or-create the daemon's identity at its conventional path and return the
/// pubkey. Called before spawn so the cross-host known-hosts files can bind the
/// real keys; the daemon re-loads this exact key on startup.
fn provision_identity(home: &Path) -> [u8; 32] {
    let id = covenant_identity::LocalIdentity::load_or_create(
        &home.join("identity").join("local.key"),
        "user@local",
    )
    .expect("load/create identity");
    id.pubkey_bytes()
}

/// Write `<home>/peers/known-hosts.json` binding a single host to an endpoint,
/// serializing the same `KnownHosts` type the daemon deserializes at startup.
fn write_known_hosts(home: &Path, host: &str, url: String, pubkey: [u8; 32]) {
    let known = covenant_peer_auth::KnownHosts::new().with_host(
        host,
        covenant_peer_auth::PeerEndpoint { url, pubkey },
    );
    let path = covenantd::known_hosts_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create peers dir");
    std::fs::write(&path, serde_json::to_vec(&known).expect("serialize known-hosts"))
        .expect("write known-hosts.json");
}

async fn spawn_daemon(home: &Path, port: u16, deliver_timeout_ms: Option<&str>) -> Child {
    let exe = env!("CARGO_BIN_EXE_covenantd");
    let mut cmd = Command::new(exe);
    cmd.env("COVENANT_HOME", home)
        .env("COVENANT_HTTP_PORT", port.to_string())
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(ms) = deliver_timeout_ms {
        cmd.env("COVENANT_A2A_DELIVER_TIMEOUT_MS", ms);
    }
    let child = cmd.spawn().expect("spawn covenantd");
    let sock = home.join("sock");
    assert!(
        wait_for_sock(&sock).await,
        "daemon never created its socket at {}",
        sock.display()
    );
    wait_for_http(&format!("http://127.0.0.1:{port}")).await;
    child
}

fn bearer_client(token: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

async fn grant(client: &reqwest::Client, base: &str, action: &str) {
    let resp: Value = client
        .post(format!("{base}/capabilities/grant"))
        .json(&json!({ "action": action }))
        .send()
        .await
        .expect("grant request")
        .json()
        .await
        .expect("grant body");
    assert_eq!(
        resp["kind"], "capability_granted",
        "grant {action} must succeed: {resp:?}",
    );
}

#[tokio::test]
#[ignore = "live: spawns two covenantd daemons on loopback and proves an authenticated cross-host A2A task sent on A is delivered to B's mailbox and never enqueued on A"]
async fn live_a2a_cross_host_task_delivers_to_remote_and_not_local() {
    let home_a = tempfile::tempdir().expect("tempdir a");
    let home_b = tempfile::tempdir().expect("tempdir b");

    let a_pubkey = provision_identity(home_a.path());
    let b_pubkey = provision_identity(home_b.path());

    let port_a = pick_free_port();
    let port_b = pick_free_port();
    let base_a = format!("http://127.0.0.1:{port_a}");
    let base_b = format!("http://127.0.0.1:{port_b}");

    // Each daemon's identity display is `user@local` (host `local`), so each
    // registers the OTHER under host `local` -> { its url, its pubkey }. Mutual
    // registration is required: A resolves B to route the send, and B resolves
    // A's sender to authorize the inbound envelope.
    write_known_hosts(home_a.path(), "local", base_b.clone(), b_pubkey);
    write_known_hosts(home_b.path(), "local", base_a.clone(), a_pubkey);

    let mut a = spawn_daemon(home_a.path(), port_a, None).await;
    let mut b = spawn_daemon(home_b.path(), port_b, None).await;

    let client_a = bearer_client(&read_operator_token(home_a.path()).await);
    let client_b = bearer_client(&read_operator_token(home_b.path()).await);

    let peer_a = AgentId::new("user@local", a_pubkey);
    // Address B by its own operator identity (display + pubkey) so B's mailbox,
    // which leases by full AgentId equality, can hand the task back.
    let recipient = AgentId::new("user@local", b_pubkey);

    // B authorizes inbound cross-host tasks from A by A's unforgeable pubkey;
    // A authorizes the outbound send to B by B's pubkey.
    grant(&client_b, &base_b, &format!("a2a.recv.{}", peer_a.pubkey_base58())).await;
    grant(
        &client_a,
        &base_a,
        &format!("a2a.send.{}", recipient.pubkey_base58()),
    )
    .await;

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: peer_a.clone(),
        recipient: recipient.clone(),
        intent_text: "cross-host loopback probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    let sent: Value = client_a
        .post(format!("{base_a}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post cross-host task")
        .json()
        .await
        .expect("send body");
    assert_eq!(
        sent["kind"], "a2_a_task_queued",
        "an authorized cross-host send the remote admits must report queued, not error: {sent:?}",
    );

    // The task must land in B's mailbox. B's admission enqueues before answering
    // 202, so it is durable by the time the send returns; poll to absorb any lag.
    let mut delivered = None;
    for _ in 0..100 {
        let next: Value = client_b
            .get(format!("{base_b}/a2a/tasks/next"))
            .send()
            .await
            .expect("poll B mailbox")
            .json()
            .await
            .expect("B mailbox body");
        assert_eq!(next["kind"], "a2_a_task_opt", "mailbox read envelope: {next:?}");
        if !next["task"].is_null() {
            delivered = Some(next);
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    let delivered = delivered.expect("cross-host task must be delivered to B's mailbox");
    let typed: A2ATask = serde_json::from_value(delivered["task"].clone())
        .expect("B mailbox row deserializes as A2ATask");
    assert_eq!(typed.id, task.id, "delivered task id must match the sent id");
    assert_eq!(
        typed.sender.pubkey, a_pubkey,
        "delivered task must name A as its sender",
    );
    assert_eq!(
        typed.recipient.pubkey, b_pubkey,
        "delivered task must be addressed to B",
    );
    assert_eq!(typed.intent_text, "cross-host loopback probe");

    // No misdelivery to local: A routed the task cross-host, so A's own mailbox
    // must be empty.
    let a_next: Value = client_a
        .get(format!("{base_a}/a2a/tasks/next"))
        .send()
        .await
        .expect("poll A mailbox")
        .json()
        .await
        .expect("A mailbox body");
    assert_eq!(a_next["kind"], "a2_a_task_opt");
    assert!(
        a_next["task"].is_null(),
        "a cross-host task must never be enqueued on the sender's local mailbox: {a_next:?}",
    );

    let _ = a.kill().await;
    let _ = b.kill().await;
}

#[tokio::test]
#[ignore = "live: spawns one covenantd daemon, sends an A2A task to a configured-but-unresponsive remote, and asserts the send fails within a bounded timeout while the daemon stays responsive"]
async fn live_a2a_cross_host_dead_remote_fails_within_bounded_timeout() {
    let home = tempfile::tempdir().expect("tempdir");
    let a_pubkey = provision_identity(home.path());

    // A black-hole remote: a bound socket that completes the TCP handshake (so the
    // connection is not refused) but is never accepted and never answers, so the
    // outbound POST hangs until the delivery timeout fires. Held in scope.
    let blackhole = std::net::TcpListener::bind("127.0.0.1:0").expect("bind black-hole");
    let dead_port = blackhole.local_addr().unwrap().port();

    let dead_pubkey = [7u8; 32];
    write_known_hosts(
        home.path(),
        "deadhost",
        format!("http://127.0.0.1:{dead_port}"),
        dead_pubkey,
    );

    let port = pick_free_port();
    let base = format!("http://127.0.0.1:{port}");
    // Override the 10s production default so a stalled remote is bounded tightly
    // and the assertion can distinguish "timeout honored" from "default" or hang.
    let mut child = spawn_daemon(home.path(), port, Some("2000")).await;

    let client = bearer_client(&read_operator_token(home.path()).await);

    let recipient = AgentId::new("agent@deadhost", dead_pubkey);
    grant(&client, &base, &format!("a2a.send.{}", recipient.pubkey_base58())).await;

    let task = A2ATask {
        id: Uuid::new_v4(),
        sender: AgentId::new("user@local", a_pubkey),
        recipient,
        intent_text: "dead remote probe".into(),
        task_kind: None,
        parent: None,
        deadline_ms: None,
        idempotency: None,
    };

    let start = Instant::now();
    let sent: Value = client
        .post(format!("{base}/a2a/tasks"))
        .json(&task)
        .send()
        .await
        .expect("post task to dead remote")
        .json()
        .await
        .expect("send body");
    let elapsed = start.elapsed();

    assert_eq!(
        sent["kind"], "error",
        "delivery to an unresponsive remote must surface as an error, not a queue: {sent:?}",
    );
    assert!(
        sent["message"]
            .as_str()
            .is_some_and(|m| m.contains("unreachable")),
        "the error must report the remote as unreachable: {sent:?}",
    );
    assert!(
        elapsed >= Duration::from_millis(1500),
        "the send must actually exercise the ~2s delivery timeout, not return instantly via a \
         connection refusal — this positively pins the timeout path; took {elapsed:?}",
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "a stalled remote must be bounded by the delivery timeout (~2s), not the 10s default or an \
         unbounded hang; took {elapsed:?}",
    );

    // The daemon must stay responsive after absorbing the stalled delivery.
    let health = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("daemon must answer /health after a stalled delivery");
    assert!(
        health.status().is_success(),
        "daemon must stay healthy after a dead-remote send",
    );

    drop(blackhole);
    let _ = child.kill().await;
}
