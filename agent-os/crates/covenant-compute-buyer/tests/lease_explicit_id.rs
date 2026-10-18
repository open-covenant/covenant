//! Fixed-ID canaries persist a single authorization before dispatch. All
//! coordinators here are local fixtures; no provider or chain is contacted.
use axum::{
    extract::{Path as RoutePath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use covenant_compute_buyer::durable_budget::DurableBudget;
use covenant_compute_protocol::SignedJobEnvelope;
use covenant_identity::LocalIdentity;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, process::Command};
use uuid::Uuid;

const ID: &str = "abcdef01-0000-4000-8000-000000000001";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
#[derive(Clone)]
struct Fixture {
    root: std::path::PathBuf,
    url: String,
    posts: Arc<AtomicUsize>,
    reserved_before_post: Arc<AtomicBool>,
    status: StatusCode,
}
impl Fixture {
    fn observe(&self, envelope: &SignedJobEnvelope) {
        envelope.verify().unwrap();
        assert_eq!(envelope.payload.job_id.to_string(), ID);
        let book = DurableBudget::open(
            &self.root,
            &envelope.payload.buyer.pubkey_base58(),
            &self.url,
            12_000,
        )
        .unwrap();
        let rows = book.pending(now()).unwrap();
        self.reserved_before_post.store(
            rows.iter().any(|r| {
                r.job_id == envelope.payload.job_id
                    && r.envelope.payload_json == envelope.payload_json
                    && r.ceiling_micro_usdc == 12_000
            }),
            Ordering::SeqCst,
        );
        self.posts.fetch_add(1, Ordering::SeqCst);
    }
}
async fn fixture(root: &Path, status: StatusCode) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture {
        root: root.into(),
        url: format!("http://{}", listener.local_addr().unwrap()),
        posts: Arc::default(),
        reserved_before_post: Arc::default(),
        status,
    };
    let router = Router::new().route("/federation/jobs", post(|State(f): State<Fixture>, Json(envelope): Json<SignedJobEnvelope>| async move {
        f.observe(&envelope);
        tokio::time::sleep(Duration::from_millis(30)).await;
        (f.status, Json(serde_json::json!({"job_id":envelope.payload.job_id,"status":"offered"}))).into_response()
    })).route("/federation/jobs/:id/lease", get(|RoutePath(id): RoutePath<String>| async move {
        Json(serde_json::json!({"job_id":id,"status":"accepted","access":null,"rate_micro_usdc_per_sec":200,"max_duration_secs":60,"elapsed_ms":0,"charged_micro_usdc":0,"close_requested":false}))
    })).with_state(f.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    f
}
fn command(url: &str, home: &Path, root: &Path, id: &str, budget: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_covenant-compute"));
    c.env_clear()
        .env("HOME", home)
        .env("COVENANT_COMPUTE_MCP_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_URL", url)
        .env("COVENANT_COMPUTE_BUDGET_HOME", root)
        .args([
            "lease",
            "open",
            "--job-id",
            id,
            "--minutes",
            "1",
            "--rate",
            "200",
            "--no-wait",
            "--json",
        ]);
    if budget {
        c.env("COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC", "12000");
    }
    c
}
fn identity(home: &Path) -> LocalIdentity {
    LocalIdentity::load_or_create(&home.join("identity.json"), "buyer@compute").unwrap()
}
fn book(f: &Fixture, buyer: &LocalIdentity) -> DurableBudget {
    DurableBudget::open(&f.root, &buyer.agent_id().pubkey_base58(), &f.url, 12_000).unwrap()
}

#[tokio::test]
async fn explicit_id_is_signed_once_and_concurrent_processes_cannot_dispatch_twice() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let buyer = identity(home.path());
    let f = fixture(root.path(), StatusCode::ACCEPTED).await;
    let (a, b) = tokio::join!(
        command(&f.url, home.path(), root.path(), ID, true).output(),
        command(&f.url, home.path(), root.path(), ID, true).output()
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(
        a.status.success(),
        b.status.success(),
        "exactly one process may authorize"
    );
    let (ok, refused) = if a.status.success() { (a, b) } else { (b, a) };
    let view: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(view["job_id"], ID);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("already reserved"));
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(f.reserved_before_post.load(Ordering::SeqCst));
    assert_eq!(
        book(&f, &buyer).status(now()).unwrap().pending_micro_usdc,
        12_000
    );
}

#[tokio::test]
async fn explicit_id_5xx_keeps_reservation_after_restart_without_another_post() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let buyer = identity(home.path());
    let f = fixture(root.path(), StatusCode::BAD_GATEWAY).await;
    let first = command(&f.url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    assert!(!first.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains(ID));
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(f.reserved_before_post.load(Ordering::SeqCst));
    let again = command(&f.url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        book(&f, &buyer).status(now()).unwrap().pending_micro_usdc,
        12_000
    );
}

#[tokio::test]
async fn explicit_id_lost_response_does_not_retry_and_keeps_authorization() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let buyer = identity(home.path());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture {
        root: root.path().into(),
        url: format!("http://{}", listener.local_addr().unwrap()),
        posts: Arc::default(),
        reserved_before_post: Arc::default(),
        status: StatusCode::ACCEPTED,
    };
    let server = f.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let server = server.clone();
            tokio::spawn(async move {
                let mut bytes = vec![];
                let mut chunk = [0; 4096];
                loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        if bytes.len() >= pos + 4 + len {
                            let envelope: SignedJobEnvelope =
                                serde_json::from_slice(&bytes[pos + 4..pos + 4 + len]).unwrap();
                            server.observe(&envelope);
                            return;
                        }
                    }
                }
            });
        }
    });
    let result = command(&f.url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    task.abort();
    assert!(!result.status.success());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(f.reserved_before_post.load(Ordering::SeqCst));
    assert_eq!(
        book(&f, &buyer).status(now()).unwrap().pending_micro_usdc,
        12_000
    );
}

#[tokio::test]
async fn explicit_id_rejects_noncanonical_ids_and_missing_budget_before_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    identity(home.path());
    let f = fixture(root.path(), StatusCode::ACCEPTED).await;
    for id in [
        "",
        "00000000-0000-0000-0000-000000000000",
        "ABCDEF01-0000-4000-8000-000000000001",
        "abcdef01000040008000000000000001",
        "urn:uuid:abcdef01-0000-4000-8000-000000000001",
        " abcdef01-0000-4000-8000-000000000001",
    ] {
        let result = command(&f.url, home.path(), root.path(), id, true)
            .output()
            .await
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("--job-id"));
    }
    let result = command(&f.url, home.path(), root.path(), ID, false)
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("DAILY_BUDGET"));
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_refusal_releases_money_but_never_reuses_the_identifier() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let buyer = identity(home.path());
    let f = fixture(root.path(), StatusCode::PAYMENT_REQUIRED).await;
    let first = command(&f.url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    assert!(!first.status.success());
    assert_eq!(
        book(&f, &buyer).status(now()).unwrap().remaining_micro_usdc,
        12_000
    );
    let again = command(&f.url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let stored = Uuid::parse_str(ID).unwrap();
    assert!(book(&f, &buyer)
        .pending(now())
        .unwrap()
        .iter()
        .all(|r| r.job_id != stored));
}

#[tokio::test]
async fn explicit_id_redirect_never_reposts_authorization_to_the_target() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let buyer = identity(home.path());
    let target = fixture(root.path(), StatusCode::ACCEPTED).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let location = format!("{}/federation/jobs", target.url);
    let app = Router::new().route(
        "/federation/jobs",
        post(move || {
            let seen = seen.clone();
            let location = location.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(axum::http::header::LOCATION, location)],
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = command(&url, home.path(), root.path(), ID, true)
        .output()
        .await
        .unwrap();
    server.abort();
    assert!(!output.status.success());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(target.posts.load(Ordering::SeqCst), 0);
    let reservation =
        DurableBudget::open(root.path(), &buyer.agent_id().pubkey_base58(), &url, 12_000).unwrap();
    assert_eq!(
        reservation.status(now()).unwrap().pending_micro_usdc,
        12_000
    );
}
