//! The node's OpenAI-compatible surface. It fronts a local llama.cpp engine and
//! attaches the served model's [`ModelIdentity`], so a caller reaching this node
//! through the gateway gets both the completion and a commitment to exactly what
//! produced it.
//!
//! `serve` binds this on `--listen`; the node then puts it behind the gateway with
//! `covenant-inferd tunnel --target <listen>`. The engine itself is llama.cpp:
//! either an already-running server (`--engine-url`) or one this command spawns and
//! supervises (`--spawn`).

use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{ACCEPT, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use covenant_inference_protocol::{ModelIdentity, SamplingParams};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Requests are buffered before forwarding; only responses stream. This caps a
/// single prompt/embedding batch well above any real chat turn without letting a
/// client force an unbounded in-memory buffer.
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const ENGINE_READY_TIMEOUT: Duration = Duration::from_secs(180);
const OWNED_BY: &str = "covenant-inference-node";

pub struct ServeConfig {
    pub listen: SocketAddr,
    /// Base URL of an already-running engine. Ignored when `spawn` is set, which
    /// derives the URL from its own port.
    pub engine_url: String,
    /// The id this node advertises for the served model, e.g. `qwen3:8b`.
    pub model: String,
    pub weights: PathBuf,
    pub quantization: String,
    pub runtime: String,
    /// Pins the engine build in the identity. Detected from `<engine-bin> --version`
    /// when absent.
    pub runtime_version: Option<String>,
    pub sampling: SamplingParams,
    pub spawn: Option<SpawnConfig>,
}

pub struct SpawnConfig {
    pub engine_bin: PathBuf,
    pub engine_port: u16,
    pub context_size: u32,
}

/// The immutable identity this surface serves, resolved once at startup.
struct ServedModel {
    id: String,
    identity: ModelIdentity,
    digest: String,
}

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    /// Engine base URL, trailing slash trimmed so endpoints join cleanly.
    engine_url: Arc<str>,
    served: Arc<ServedModel>,
}

pub async fn run(config: ServeConfig) -> anyhow::Result<()> {
    let engine_bin = config
        .spawn
        .as_ref()
        .map(|spawn| spawn.engine_bin.clone())
        .unwrap_or_else(|| PathBuf::from("llama-server"));

    let runtime_version = match config.runtime_version {
        Some(version) => version,
        None => detect_runtime_version(&engine_bin)
            .await
            .unwrap_or_else(|| {
                tracing::warn!("could not read engine version; identity runtime_version=unknown");
                "unknown".to_owned()
            }),
    };

    tracing::info!(weights = %config.weights.display(), "hashing model weights");
    let weights_hash = hash_weights(&config.weights).await?;

    let identity = ModelIdentity {
        weights_hash,
        quantization: config.quantization,
        runtime: config.runtime,
        runtime_version,
        sampling_params: config.sampling,
    };
    tracing::info!(model = %config.model, digest = %identity.digest(), "resolved served model identity");

    let engine_url = match &config.spawn {
        Some(spawn) => format!("http://127.0.0.1:{}", spawn.engine_port),
        None => config.engine_url.trim_end_matches('/').to_owned(),
    };

    let app = router(engine_url.clone(), config.model, identity);

    // Spawn the engine before binding so a slow model load fails startup rather
    // than surfacing as a live-but-broken node.
    let mut engine = match config.spawn {
        Some(spawn) => Some(spawn_engine(&engine_bin, &config.weights, &spawn).await?),
        None => None,
    };
    if engine.is_some() {
        wait_for_engine(&reqwest::Client::new(), &engine_url, ENGINE_READY_TIMEOUT).await?;
    }

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("bind serve listener {}", config.listen))?;
    tracing::info!(listen = %config.listen, %engine_url, "serving covenant inference surface");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    // Fall over if the engine dies under us; a node with a dead engine should stop
    // advertising, not keep answering with 502s. Exit non-zero so a supervisor set
    // to Restart=on-failure brings it back instead of treating it as a clean stop.
    let engine_died = match engine.as_mut() {
        Some(child) => tokio::select! {
            result = server => { result.context("serve loop")?; false }
            status = child.wait() => { tracing::error!(?status, "inference engine exited"); true }
        },
        None => {
            server.await.context("serve loop")?;
            false
        }
    };

    if let Some(mut child) = engine {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    if engine_died {
        anyhow::bail!("inference engine exited");
    }
    Ok(())
}

/// Wires the router around a resolved identity and a fresh engine client. Public
/// so tests can drive every endpoint against a mock engine without a real
/// llama.cpp process.
pub fn router(engine_url: String, model: String, identity: ModelIdentity) -> Router {
    let digest = identity.digest();
    let state = AppState {
        // No total timeout: SSE completions are long-lived. A read timeout instead
        // bounds the idle gap between bytes, so an engine that accepted the
        // connection then wedged can't pin this request (and its tunnel slot)
        // forever, while a slow-but-progressing generation is left alone.
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(120))
            .build()
            .expect("build engine client"),
        engine_url: Arc::from(engine_url.trim_end_matches('/')),
        served: Arc::new(ServedModel {
            id: model,
            identity,
            digest,
        }),
    };

    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(models))
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state)
}

async fn chat_completions(state: State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    proxy(state.0, "v1/chat/completions", headers, body).await
}

async fn embeddings(state: State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    proxy(state.0, "v1/embeddings", headers, body).await
}

/// Forwards a request to the backing engine and streams its response straight
/// back. Streaming the body unmodified is what makes one path serve both plain
/// JSON and SSE: chunks are relayed as they arrive, so `stream: true` just works.
async fn proxy(state: AppState, path: &str, headers: HeaderMap, body: Bytes) -> Response {
    let url = format!("{}/{path}", state.engine_url);
    let mut request = state.client.post(&url).body(body);
    if let Some(content_type) = headers.get(CONTENT_TYPE) {
        request = request.header(CONTENT_TYPE, content_type.clone());
    }
    if let Some(accept) = headers.get(ACCEPT) {
        request = request.header(ACCEPT, accept.clone());
    }

    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, %url, "engine request failed");
            return (StatusCode::BAD_GATEWAY, "inference engine unreachable").into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(CONTENT_TYPE, content_type);
    }
    builder
        .body(Body::from_stream(upstream.bytes_stream()))
        .expect("valid proxied response")
}

async fn models(State(state): State<AppState>) -> Json<ModelsResponse> {
    let served = &state.served;
    Json(ModelsResponse {
        object: "list",
        data: vec![ModelEntry {
            id: served.id.clone(),
            object: "model",
            owned_by: OWNED_BY,
            model_identity: served.identity.clone(),
            model_identity_digest: served.digest.clone(),
        }],
    })
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        engine_ready: engine_ready(&state).await,
        model_identity_digest: state.served.digest.clone(),
    })
}

/// Cheap liveness of the backing engine: a 200 from its `/health`, or failing
/// that its `/v1/models` (some builds expose only the latter).
async fn engine_ready(state: &AppState) -> bool {
    for path in ["health", "v1/models"] {
        let url = format!("{}/{path}", state.engine_url);
        let probe = state.client.get(&url).timeout(ENGINE_PROBE_TIMEOUT).send();
        if let Ok(response) = probe.await {
            if response.status().is_success() {
                return true;
            }
        }
    }
    false
}

#[derive(Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    owned_by: &'static str,
    model_identity: ModelIdentity,
    model_identity_digest: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    engine_ready: bool,
    model_identity_digest: String,
}

/// Streams the weights file through SHA-256 without loading it into memory - the
/// GGUF is multiple gigabytes, so this runs on a blocking thread and reads in 1 MiB
/// chunks.
pub async fn hash_weights(path: &Path) -> anyhow::Result<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("open weights {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 1 << 20];
        let mut total = 0_u64;
        loop {
            let read = file.read(&mut buffer).context("read weights")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            total += read as u64;
        }
        tracing::info!(
            bytes = total,
            elapsed_ms = started.elapsed().as_millis(),
            "hashed model weights"
        );
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .context("weights hashing task")?
}

/// Parses `llama-server --version` (`version: 9410 (031ddb2e0)`) into a build tag
/// for the identity. Best-effort: an operator can always pin `--runtime-version`.
async fn detect_runtime_version(engine_bin: &Path) -> Option<String> {
    let output = tokio::process::Command::new(engine_bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let rest = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("version:"))?
        .trim();
    let mut parts = rest.split_whitespace();
    let build = parts.next()?;
    match parts.next().map(|commit| commit.trim_matches(['(', ')'])) {
        Some(commit) => Some(format!("b{build} ({commit})")),
        None => Some(format!("b{build}")),
    }
}

async fn spawn_engine(
    engine_bin: &Path,
    weights: &Path,
    spawn: &SpawnConfig,
) -> anyhow::Result<tokio::process::Child> {
    tracing::info!(engine = %engine_bin.display(), port = spawn.engine_port, "spawning inference engine");
    tokio::process::Command::new(engine_bin)
        .arg("-m")
        .arg(weights)
        .arg("--port")
        .arg(spawn.engine_port.to_string())
        .arg("-c")
        .arg(spawn.context_size.to_string())
        .arg("--parallel")
        .arg("1")
        // Reap the engine if this process dies for any reason we don't handle.
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn engine {}", engine_bin.display()))
}

/// Polls the engine's `/health` until it loads the model or the deadline passes.
async fn wait_for_engine(
    client: &reqwest::Client,
    engine_url: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let health = format!("{}/health", engine_url.trim_end_matches('/'));
    let mut announced = false;
    loop {
        let probe = client.get(&health).timeout(ENGINE_PROBE_TIMEOUT).send();
        if let Ok(response) = probe.await {
            if response.status().is_success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("engine at {engine_url} was not ready within {timeout:?}");
        }
        if !announced {
            tracing::info!(%engine_url, "waiting for engine to load model");
            announced = true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
