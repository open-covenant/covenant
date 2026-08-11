use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use covenant_inference_gateway::pool::TunnelPool;
use covenant_inference_gateway::receipts::ReceiptStore;
use covenant_inference_gateway::registry::NodeRegistry;
use covenant_inference_gateway::server::{router, Gateway, Metering, Transport};
use covenant_inference_gateway::tunnel;
use covenant_inference_gateway::x402::{DevPrefundedVerifier, PaymentVerifier, SolanaUsdcVerifier};
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
    /// Gate the paid routes on a verified `X-PAYMENT`. Off by default: routing and
    /// receipts (accounting mode) work without it.
    #[arg(long)]
    require_payment: bool,
    /// Price per request, atomic units of the rail asset, advertised in the `402`.
    #[arg(long, default_value_t = 1_000)]
    price_micro: u64,
    /// Destination address the `402` tells payers to pay. Required when
    /// `--require-payment` is set; there is deliberately no default payment
    /// destination.
    #[arg(long)]
    pay_to: Option<String>,
    /// CAIP-2 network the `402` advertises. Devnet by default; no mainnet rail ships.
    #[arg(long, default_value = "solana:devnet")]
    rail_network: String,
    /// Asset the price is denominated in (mint/contract). Devnet USDC by default.
    #[arg(long, default_value = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU")]
    rail_asset: String,
    /// Dev prefunded allow-token the `DevPrefundedVerifier` accepts. Required when
    /// `--require-payment` is set; there is no real-settlement verifier to fall back
    /// to. Real-money settlement is a gated seam.
    #[arg(long)]
    dev_payment_token: Option<String>,
    /// Append every receipt (hashes and accounting only) to this file as JSON lines.
    #[arg(long)]
    receipts_log: Option<PathBuf>,
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

    let receipts = match cli.receipts_log.as_deref() {
        Some(path) => Arc::new(ReceiptStore::with_log(path)?),
        None => Arc::new(ReceiptStore::in_memory()),
    };

    let verifier: Arc<dyn PaymentVerifier> = if cli.require_payment {
        let token = cli.dev_payment_token.clone().context(
            "--require-payment needs --dev-payment-token (the dev prefunded allow-token); \
             real-money settlement is gated",
        )?;
        tracing::warn!("payment gate on: dev prefunded verifier, no real settlement");
        Arc::new(DevPrefundedVerifier::new(token))
    } else {
        Arc::new(SolanaUsdcVerifier::gated())
    };

    let pay_to = if cli.require_payment {
        cli.pay_to.clone().context(
            "--require-payment needs --pay-to (the payment destination); there is no default",
        )?
    } else {
        cli.pay_to.clone().unwrap_or_default()
    };
    let metering = Metering {
        require_payment: cli.require_payment,
        price_micro: cli.price_micro,
        pay_to,
        rail_network: cli.rail_network.clone(),
        rail_asset: cli.rail_asset.clone(),
    };

    let gateway = Gateway::new(registry, transport.clone())
        .with_metering(metering)
        .with_verifier(verifier)
        .with_receipts(receipts);
    let app = router(gateway);
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
