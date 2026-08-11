use anyhow::{anyhow, Context};
use axum::body::{Body, Bytes};
use axum::http::header::{ACCEPT, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use covenant_inference_node::frame::{read_frame, write_frame};
use covenant_inference_node::tunnel::{RelayOpen, RelayReady, RelayService};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;

use crate::pool::{PooledStream, TunnelStream};
use crate::registry::CatalogEntry;

/// HTTP/1.1 requires a Host; the node's surface ignores its value, so a stable
/// placeholder keeps requests well-formed without leaking anything routable.
const HOST_HEADER: &str = "covenant-inference-node";

/// A node's reply, fully buffered. The metering layer needs the whole body in hand
/// to hash it into a receipt before the response leaves the gateway, so the paid
/// routes collect rather than stream. `stream: true` still returns the full SSE
/// body, just not incrementally.
pub struct CollectedResponse {
    pub status: StatusCode,
    pub content_type: Option<HeaderValue>,
    pub body: Bytes,
}

impl CollectedResponse {
    pub fn into_response(self) -> Response {
        let mut builder = Response::builder().status(self.status);
        if let Some(content_type) = self.content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        builder
            .body(Body::from(self.body))
            .expect("status and headers are valid")
    }
}

/// Proxies a buffered client request down a node connection and returns the reply
/// fully collected.
///
/// `relay` distinguishes the two transports the gateway supports over the same
/// [`PooledStream`]: a tunnel connection that must first be opened with a relay
/// handshake, versus a direct dev connection straight to a node's OpenAI surface.
pub async fn relay_collect(
    stream: PooledStream,
    relay: bool,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> anyhow::Result<CollectedResponse> {
    let sender = open(stream, relay).await.context("open node connection")?;
    forward(sender, method, path, headers, body).await
}

/// Buffered proxy that maps any failure to a `502`. Kept for callers that only need
/// the response passed through, notably the relay loopback coverage.
pub async fn relay_request(
    stream: PooledStream,
    relay: bool,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    match relay_collect(stream, relay, method, path, headers, body).await {
        Ok(collected) => collected.into_response(),
        Err(error) => {
            tracing::warn!(%error, "proxy request to node");
            (StatusCode::BAD_GATEWAY, "node request failed").into_response()
        }
    }
}

/// Probes a node's `/v1/models` over one of its connections so the gateway can route
/// friendly ids and answer its own `/v1/models` from what nodes actually serve.
pub async fn fetch_catalog(stream: PooledStream, relay: bool) -> anyhow::Result<Vec<CatalogEntry>> {
    let mut sender = open(stream, relay).await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/v1/models")
        .header(HOST, HOST_HEADER)
        .body(Full::new(Bytes::new()))
        .context("build node models request")?;
    let response = sender
        .send_request(request)
        .await
        .context("request node models")?;
    if !response.status().is_success() {
        anyhow::bail!("node models returned {}", response.status());
    }
    let body = response
        .into_body()
        .collect()
        .await
        .context("read node models")?
        .to_bytes();
    let models: NodeModels = serde_json::from_slice(&body).context("decode node models")?;
    Ok(models
        .data
        .into_iter()
        .map(|entry| CatalogEntry {
            id: entry.id,
            digest: entry.model_identity_digest,
        })
        .collect())
}

async fn open(
    stream: PooledStream,
    relay: bool,
) -> anyhow::Result<http1::SendRequest<Full<Bytes>>> {
    if relay {
        open_relay(stream).await
    } else {
        handshake(stream).await
    }
}

/// Opens the node's inference relay on a freshly-taken tunnel connection, then runs
/// a bare HTTP/1 client over it. Once the relay is live the pooled stream terminates
/// at the node's OpenAI surface, so this reaches a node that only ever dialed *out*.
/// `reqwest` can't stand in here — it owns its connector and won't speak HTTP over a
/// socket handed to it.
async fn open_relay(mut stream: PooledStream) -> anyhow::Result<http1::SendRequest<Full<Bytes>>> {
    write_frame(
        &mut stream,
        &RelayOpen {
            service: RelayService::Inference,
        },
    )
    .await
    .context("send relay open")?;
    let ready: RelayReady = read_frame(&mut stream).await.context("read relay ready")?;
    if !ready.ready {
        return Err(anyhow!(
            "node relay unavailable: {}",
            ready.error.as_deref().unwrap_or("unknown")
        ));
    }
    handshake(stream).await
}

async fn handshake<S: TunnelStream>(stream: S) -> anyhow::Result<http1::SendRequest<Full<Bytes>>> {
    let (sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .context("http/1 handshake over node connection")?;
    // The connection future drives the byte pump for this one request; it ends when
    // the response body is done and the stream closes.
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(%error, "node connection closed");
        }
    });
    Ok(sender)
}

async fn forward(
    mut sender: http1::SendRequest<Full<Bytes>>,
    method: Method,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> anyhow::Result<CollectedResponse> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, HOST_HEADER);
    if let Some(content_type) = headers.get(CONTENT_TYPE) {
        builder = builder.header(CONTENT_TYPE, content_type.clone());
    }
    if let Some(accept) = headers.get(ACCEPT) {
        builder = builder.header(ACCEPT, accept.clone());
    }
    let request = builder
        .body(Full::new(body))
        .context("build node request")?;

    let upstream = sender
        .send_request(request)
        .await
        .context("send node request")?;
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let body = upstream
        .into_body()
        .collect()
        .await
        .context("read node response")?
        .to_bytes();

    Ok(CollectedResponse {
        status,
        content_type,
        body,
    })
}

#[derive(serde::Deserialize)]
struct NodeModels {
    data: Vec<NodeModelEntry>,
}

#[derive(serde::Deserialize)]
struct NodeModelEntry {
    id: String,
    model_identity_digest: String,
}
