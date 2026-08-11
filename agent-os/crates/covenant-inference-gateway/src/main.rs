use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use covenant_inference_gateway::pool::TunnelPool;
use covenant_inference_gateway::registry::NodeRegistry;
use covenant_inference_gateway::server::{router, Gateway, Transport};
use covenant_inference_gateway::tunnel;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "covenant-inference-gateway",
    about = "Routing gateway for the Covenant verifiable inference network"
)]
struct Cli {
    /// OpenAI-compatible client surface plus the node enrol/heartbeat endpoints.
    #[arg(long, default_value = "127.0.0.1:8080")]
    http_listen: SocketAddr,
    /// mTLS listener nodes dial outbound to park reverse tunnels. Unused with --node-url.
    #[arg(long, default_value = "127.0.0.1:7443")]
    tunnel_listen: SocketAddr,
    /// Gateway server certificate presented on the tunnel.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// CA that signed the node client certificates the tunnel requires.
    #[arg(long)]
    client_ca: Option<PathBuf>,
    /// Dev route: proxy straight to a node's OpenAI surface at this `host:port`
    /// instead of over a tunnel. Bypasses mTLS; for local end-to-end runs only.
    #[arg(long)]
    node_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    let cli = Cli::parse();
    let registry = Arc::new(NodeRegistry::default());

    let transport = match cli.node_url.as_deref() {
        Some(url) => {
            tracing::warn!(%url, "serving in direct dev mode: tunnel mTLS is disabled");
            Transport::Direct(Arc::from(direct_target(url)))
        }
        None => Transport::Tunnel(TunnelPool::default()),
    };

    let app = router(Gateway::new(registry, transport.clone()));
    let http = tokio::net::TcpListener::bind(cli.http_listen)
        .await
        .with_context(|| format!("bind http listener {}", cli.http_listen))?;
    tracing::info!(listen = %cli.http_listen, "gateway http surface listening");
    let serve = axum::serve(http, app).with_graceful_shutdown(shutdown());

    match transport {
        Transport::Tunnel(pool) => {
            let (cert, key, ca) = tls_paths(&cli)?;
            let acceptor = tunnel::tls_acceptor(&cert, &key, &ca)?;
            tokio::select! {
                result = serve => result.context("gateway http server")?,
                result = tunnel::serve(cli.tunnel_listen, acceptor, pool) => {
                    result.context("tunnel listener")?;
                }
            }
        }
        Transport::Direct(_) => serve.await.context("gateway http server")?,
    }
    Ok(())
}

/// Accepts a bare `host:port` or an `http://host:port` URL and returns the
/// `host:port` the direct proxy dials.
fn direct_target(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_owned()
}

fn tls_paths(cli: &Cli) -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
    match (&cli.tls_cert, &cli.tls_key, &cli.client_ca) {
        (Some(cert), Some(key), Some(ca)) => Ok((cert.clone(), key.clone(), ca.clone())),
        _ => anyhow::bail!(
            "tunnel mode needs --tls-cert, --tls-key and --client-ca (or pass --node-url for the dev route)"
        ),
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
