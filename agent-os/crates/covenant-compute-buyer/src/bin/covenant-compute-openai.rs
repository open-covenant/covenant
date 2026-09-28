//! `covenant-compute-openai` — an OpenAI-compatible HTTP front door for
//! the Covenant compute network. Point any OpenAI client (the OpenAI
//! SDKs, LangChain, `curl`) at this server's base URL and every
//! `POST /v1/chat/completions`, legacy `POST /v1/completions`, and
//! `POST /v1/embeddings` is bought on the network, paid, and returned with
//! the operator's signed, locally re-verified receipt — no client code
//! changes.
//!
//! Configuration is all environment: `COVENANT_COMPUTE_COORDINATOR_URL`
//! (required) is the trust anchor; `COVENANT_COMPUTE_OPENAI_BIND`
//! (default `127.0.0.1:8787`) is the listen address;
//! `COVENANT_COMPUTE_OPENAI_API_KEY`, when set, is the bearer token every
//! `/v1/*` request must present. Spend guards match the MCP server:
//! `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC` caps any one call,
//! `COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC` caps one server run's total.
//! Identity persists under `COVENANT_COMPUTE_OPENAI_HOME`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use covenant_compute_buyer::{
    anthropic_router, gemini_router, http_client, openai_router, BuyerConfig, OpenAiState,
    SpendCaps,
};
use covenant_identity::LocalIdentity;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
covenant-compute-openai — an HTTP endpoint selling Covenant compute network
inference. Serves the OpenAI API (chat behind POST /v1/chat/completions, the
legacy POST /v1/completions, embeddings behind POST /v1/embeddings, audio) and
the Anthropic Messages API behind POST /v1/messages, so a client built for
either points its base URL here unchanged.

Usage:
  covenant-compute-openai           serve the endpoint
  covenant-compute-openai --version print the version

Requires COVENANT_COMPUTE_COORDINATOR_URL. Listens on
COVENANT_COMPUTE_OPENAI_BIND (default 127.0.0.1:8787). Set
COVENANT_COMPUTE_OPENAI_API_KEY to require an API key (sent as an OpenAI
bearer token or an Anthropic x-api-key header). Spend guards:
COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC caps any one call;
COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC caps how much one server run
commits before it refuses, and resets on restart. Identity lives under
COVENANT_COMPUTE_OPENAI_HOME. The full knob table is in the
covenant-compute-buyer README.
";

fn env_or<T: std::str::FromStr>(key: &str, default: T, hint: &str) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} {hint} (got {v:?}): {e}")),
        _ => Ok(default),
    }
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn home() -> anyhow::Result<PathBuf> {
    if let Some(p) = optional_env("COVENANT_COMPUTE_OPENAI_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".covenant-compute-openai"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "unexpected argument {other:?} — this binary takes none; run `--help` for the summary"
        ),
        None => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "covenant_compute_openai=info".into()),
        )
        .init();

    let coordinator_url = optional_env("COVENANT_COMPUTE_COORDINATOR_URL")
        .context("COVENANT_COMPUTE_COORDINATOR_URL must be set and non-empty")?;
    anyhow::ensure!(
        coordinator_url.starts_with("http://") || coordinator_url.starts_with("https://"),
        "COVENANT_COMPUTE_COORDINATOR_URL must start with http:// or https:// (got \
         {coordinator_url:?})"
    );

    let home = home()?;
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    let identity = LocalIdentity::load_or_create(&home.join("identity.json"), "buyer@compute")
        .context("load or create buyer identity")?;

    // Spend guards fail the boot on a typo rather than silently widening
    // to a permissive default and serving under a ceiling never set.
    let max_price_micro_usdc = env_or(
        "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC",
        1_000_000u64,
        "must be a whole number of micro-USDC",
    )?;
    let default_deadline_ms = env_or(
        "COVENANT_COMPUTE_DEADLINE_MS",
        60_000u64,
        "must be a whole number of milliseconds",
    )?;
    let max_total_micro_usdc: Option<u64> =
        match std::env::var("COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC") {
            Ok(v) if !v.trim().is_empty() => Some(v.trim().parse().map_err(|e| {
                anyhow::anyhow!(
                    "COVENANT_COMPUTE_MAX_TOTAL_MICRO_USDC must be a whole number of micro-USDC \
                 (got {v:?}): {e}"
                )
            })?),
            _ => None,
        };
    let bind =
        optional_env("COVENANT_COMPUTE_OPENAI_BIND").unwrap_or_else(|| "127.0.0.1:8787".into());
    let api_key = optional_env("COVENANT_COMPUTE_OPENAI_API_KEY");

    let state = Arc::new(OpenAiState {
        http: http_client(),
        buyer: BuyerConfig {
            coordinator_url: coordinator_url.clone(),
            poll_interval: Duration::from_millis(500),
            referral_code: optional_env("COVENANT_COMPUTE_REFERRAL_CODE"),
            rpc_url: optional_env("COVENANT_COMPUTE_RPC_URL"),
        },
        identity,
        caps: Arc::new(SpendCaps::new(max_price_micro_usdc, max_total_micro_usdc)),
        default_deadline_ms,
        api_key: api_key.clone(),
    });

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read the bound address")?;
    tracing::info!(
        pubkey = %bs58::encode(state.identity.pubkey_bytes()).into_string(),
        coordinator = %coordinator_url,
        %addr,
        auth = if api_key.is_some() { "bearer" } else { "open" },
        "covenant-compute-openai ready"
    );

    // SIGTERM is how a supervisor (docker stop, systemd, a PaaS deploy)
    // asks the front door to stop. An in-flight request is a buy the client
    // is already being charged for, so draining lets it return the paid-for
    // completion instead of dropping the connection, and the exit reads
    // clean.
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {},
                _ = term.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        tracing::info!("shutdown signal received; draining in-flight requests");
    };

    let router = openai_router(Arc::clone(&state))
        .merge(anthropic_router(Arc::clone(&state)))
        .merge(gemini_router(state));
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .context("serve the openai endpoint")?;
    Ok(())
}
