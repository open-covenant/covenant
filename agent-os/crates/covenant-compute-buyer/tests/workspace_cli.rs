//! Runs the shipped CLI in separate processes against the real workspace
//! router and a conditional-write object fixture. No chain or GPU is charged.
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::put,
    Router,
};
use base64::Engine;
use covenant_compute_coordinator::workspace::{
    router, S3WorkspaceConfig, S3WorkspaceObjects, WorkspaceService,
};
use covenant_compute_coordinator::{CoordinatorConfig, CoordinatorState, MockPayout, NoReputation};
use covenant_identity::LocalIdentity;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

#[derive(Clone, Default)]
struct Objects {
    entries: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    lose_put_reply: Arc<AtomicBool>,
    put_count: Arc<AtomicUsize>,
}

async fn object_put(
    State(objects): State<Objects>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    objects.put_count.fetch_add(1, Ordering::SeqCst);
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&body));
    let conditional = headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some("*");
    if (!conditional && !body.is_empty())
        || headers
            .get("x-amz-checksum-sha256")
            .and_then(|v| v.to_str().ok())
            != Some(digest.as_str())
    {
        return StatusCode::BAD_REQUEST;
    }
    let mut entries = objects.entries.lock();
    if conditional && entries.contains_key(&key) {
        return StatusCode::PRECONDITION_FAILED;
    }
    entries.insert(key, body.to_vec());
    if objects.lose_put_reply.swap(false, Ordering::SeqCst) {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::OK
    }
}

async fn object_get(State(objects): State<Objects>, Path(key): Path<String>) -> Response {
    let Some(bytes) = objects.entries.lock().get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&bytes));
    (
        [
            ("x-amz-checksum-sha256", digest),
            ("content-length", bytes.len().to_string()),
        ],
        bytes,
    )
        .into_response()
}

async fn object_delete(State(objects): State<Objects>, Path(key): Path<String>) -> StatusCode {
    objects.entries.lock().remove(&key);
    StatusCode::NO_CONTENT
}

async fn cli(home: &std::path::Path, endpoint: &str, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_covenant-compute"))
        .env("COVENANT_COMPUTE_MCP_HOME", home)
        .env("COVENANT_COMPUTE_COORDINATOR_URL", endpoint)
        .args(["--json", "workspace"])
        .args(args)
        .output()
        .await
        .unwrap()
}

fn okay(output: std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn cli_save_restart_retry_restore_and_private_backup_preserve_one_version() {
    let objects = Objects::default();
    let storage = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage_url = format!("http://{}", storage.local_addr().unwrap());
    let object_app = Router::new()
        .route(
            "/bucket/*key",
            put(object_put).get(object_get).delete(object_delete),
        )
        .with_state(objects.clone());
    let storage_task = tokio::spawn(async move {
        axum::serve(storage, object_app).await.unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let state = CoordinatorState::new(
        LocalIdentity::generate("workspace@coordinator"),
        CoordinatorConfig::default(),
        Arc::new(NoReputation),
        Arc::new(MockPayout::new()),
        Arc::new(covenant_audit::InMemoryAuditLog::new()),
    );
    let backend = S3WorkspaceObjects::new(S3WorkspaceConfig {
        endpoint: storage_url,
        region: "auto".into(),
        bucket: "bucket".into(),
        prefix: "snapshots".into(),
        access_key: "fixture".into(),
        secret_key: "fixture-secret".into(),
        session_token: None,
    })
    .unwrap();
    let service = Arc::new(
        WorkspaceService::open(
            &home.path().join("workspaces.jsonl"),
            Arc::new(backend),
            state,
            4,
        )
        .unwrap(),
    );
    let coordinator = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", coordinator.local_addr().unwrap());
    let app = router(service.clone());
    let server_task = tokio::spawn(async move {
        axum::serve(coordinator, app).await.unwrap();
    });
    let buyer = tempfile::tempdir().unwrap();
    let initial = okay(cli(buyer.path(), &endpoint, &["create", "training-checkpoint"]).await);
    let id = initial["id"].as_str().unwrap();
    let source = home.path().join("checkpoint.tar");
    let plain = b"private checkpoint bytes, training step 1200";
    std::fs::write(&source, plain).unwrap();
    objects.lose_put_reply.store(true, Ordering::SeqCst);
    let failed = cli(
        buyer.path(),
        &endpoint,
        &["save", id, source.to_str().unwrap()],
    )
    .await;
    assert!(!failed.status.success());
    assert_eq!(
        objects.entries.lock().len(),
        1,
        "write committed despite lost success reply"
    );
    assert!(objects
        .entries
        .lock()
        .values()
        .all(|v| !v.windows(plain.len()).any(|w| w == plain)));
    // A second save is refused locally, before granting or uploading again.
    let again = cli(
        buyer.path(),
        &endpoint,
        &["save", id, source.to_str().unwrap()],
    )
    .await;
    assert!(!again.status.success());
    assert_eq!(objects.put_count.load(Ordering::SeqCst), 1);
    let committed = okay(cli(buyer.path(), &endpoint, &["retry", id]).await);
    assert_eq!(committed["version"], 1);
    assert_eq!(
        objects.entries.lock().len(),
        1,
        "new process uses the original immutable object"
    );
    assert_eq!(objects.put_count.load(Ordering::SeqCst), 2);
    let restored = home.path().join("restored.tar");
    okay(
        cli(
            buyer.path(),
            &endpoint,
            &["restore", id, restored.to_str().unwrap()],
        )
        .await,
    );
    assert_eq!(std::fs::read(&restored).unwrap(), plain);
    let exists = cli(
        buyer.path(),
        &endpoint,
        &["restore", id, restored.to_str().unwrap()],
    )
    .await;
    assert!(!exists.status.success());
    assert_eq!(std::fs::read(&restored).unwrap(), plain);
    let backup = home.path().join("private-backup.json");
    okay(
        cli(
            buyer.path(),
            &endpoint,
            &["backup", id, backup.to_str().unwrap()],
        )
        .await,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let later_home = tempfile::tempdir().unwrap();
    std::fs::copy(
        buyer.path().join("identity.json"),
        later_home.path().join("identity.json"),
    )
    .unwrap();
    okay(
        cli(
            later_home.path(),
            &endpoint,
            &["import", backup.to_str().unwrap()],
        )
        .await,
    );
    let later_restore = home.path().join("later-restored.tar");
    okay(
        cli(
            later_home.path(),
            &endpoint,
            &["restore", id, later_restore.to_str().unwrap()],
        )
        .await,
    );
    assert_eq!(std::fs::read(later_restore).unwrap(), plain);
    let foreign_home = tempfile::tempdir().unwrap();
    assert!(!cli(
        foreign_home.path(),
        &endpoint,
        &["import", backup.to_str().unwrap()]
    )
    .await
    .status
    .success());
    okay(cli(buyer.path(), &endpoint, &["delete", id]).await);
    assert!(!cli(
        buyer.path(),
        &endpoint,
        &["restore", id, source.to_str().unwrap()]
    )
    .await
    .status
    .success());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    service.cleanup(now + 500_000).await.unwrap();
    assert!(!objects.entries.lock().is_empty());
    assert!(objects.entries.lock().values().all(Vec::is_empty));
    server_task.abort();
    storage_task.abort();
}
