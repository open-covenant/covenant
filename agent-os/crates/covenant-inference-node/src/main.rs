use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use covenant_inference_node::daemon::{self, EnrollArgs, HeartbeatArgs};
use covenant_inference_node::target::{InferenceTarget, LocalHttpTarget};
use covenant_inference_node::tunnel::{self, TunnelConfig};
use covenant_inference_protocol::{ModelIdentity, SamplingParams};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "covenant-inferd", about = "Covenant inference node daemon")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a device identity keypair.
    CreateIdentity {
        #[arg(long, default_value = "/var/lib/covenant-inferd/device.json")]
        path: PathBuf,
    },
    /// Register the node and the model it serves with the control plane.
    Enroll {
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        control_plane: String,
        #[arg(long)]
        operator_wallet: String,
        #[arg(long)]
        payout_wallet: String,
        #[arg(long)]
        weights_hash: String,
        #[arg(long)]
        quantization: String,
        #[arg(long, default_value = "llama.cpp")]
        runtime: String,
        #[arg(long)]
        runtime_version: String,
        #[arg(long, default_value_t = 0.7)]
        temperature: f64,
        #[arg(long, default_value_t = 0.95)]
        top_p: f64,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long, default_value_t = 512)]
        sampling_max_tokens: u32,
        #[arg(long)]
        max_tokens_per_second: u32,
        #[arg(long)]
        rate_per_second: u64,
    },
    /// Publish signed telemetry to the control plane on a fixed cadence.
    Heartbeat {
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        control_plane: String,
        #[arg(long, default_value_t = 0)]
        active_sessions: u32,
        #[arg(long, default_value_t = 0)]
        tokens_per_second: u32,
        #[arg(long, default_value_t = false)]
        tunnel_connected: bool,
        #[arg(long)]
        served_model_digest: Option<String>,
        #[arg(long, default_value_t = 15)]
        interval_seconds: u64,
    },
    /// Hold the outbound reverse tunnel open and relay to the local inference target.
    Tunnel {
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        gateway: String,
        #[arg(long)]
        server_name: String,
        #[arg(long)]
        ca_certificate: PathBuf,
        #[arg(long)]
        client_certificate: PathBuf,
        #[arg(long)]
        client_key: PathBuf,
        #[arg(long)]
        connection_id: String,
        #[arg(long, default_value = "127.0.0.1:8000")]
        target: SocketAddr,
        #[arg(long, default_value_t = 8)]
        slots: u16,
    },
    /// Probe the local inference target's health.
    Health {
        #[arg(long, default_value = "127.0.0.1:8000")]
        target: SocketAddr,
    },
    /// Serve the local model engine (seam; not yet wired).
    Serve {
        #[arg(long, default_value = "127.0.0.1:8000")]
        listen: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();
    dispatch(Cli::parse().command).await
}

async fn dispatch(command: Command) -> anyhow::Result<()> {
    match command {
        Command::CreateIdentity { path } => {
            println!("{}", daemon::create_identity(&path)?);
            Ok(())
        }
        Command::Enroll {
            identity,
            control_plane,
            operator_wallet,
            payout_wallet,
            weights_hash,
            quantization,
            runtime,
            runtime_version,
            temperature,
            top_p,
            seed,
            sampling_max_tokens,
            max_tokens_per_second,
            rate_per_second,
        } => {
            daemon::enroll(EnrollArgs {
                identity,
                control_plane,
                operator_wallet,
                payout_wallet,
                model: ModelIdentity {
                    weights_hash,
                    quantization,
                    runtime,
                    runtime_version,
                    sampling_params: SamplingParams {
                        temperature,
                        top_p,
                        seed,
                        max_tokens: sampling_max_tokens,
                    },
                },
                max_tokens_per_second,
                rate_per_second,
            })
            .await
        }
        Command::Heartbeat {
            identity,
            control_plane,
            active_sessions,
            tokens_per_second,
            tunnel_connected,
            served_model_digest,
            interval_seconds,
        } => {
            daemon::heartbeat(HeartbeatArgs {
                identity,
                control_plane,
                active_sessions,
                tokens_per_second,
                tunnel_connected,
                served_model_digest,
                interval: Duration::from_secs(interval_seconds),
            })
            .await
        }
        Command::Tunnel {
            identity,
            gateway,
            server_name,
            ca_certificate,
            client_certificate,
            client_key,
            connection_id,
            target,
            slots,
        } => {
            let signing_key = daemon::load_signing_key(&identity)?;
            tunnel::run(
                TunnelConfig {
                    gateway,
                    server_name,
                    ca_certificate,
                    client_certificate,
                    client_key,
                    connection_id,
                    slots,
                },
                LocalHttpTarget::new(target),
                signing_key,
            )
            .await
        }
        Command::Health { target } => {
            LocalHttpTarget::new(target).health().await?;
            println!("ok");
            Ok(())
        }
        Command::Serve { listen } => {
            // Seam: `serve` will bind an OpenAI-compatible HTTP server backed by
            // llama.cpp or vLLM on `listen`, then the node relays it with
            // `tunnel --target <listen>`. That engine integration is out of scope
            // for the daemon — implement the server here and point the tunnel at
            // it. See src/target.rs for the transport-side contract.
            anyhow::bail!(
                "covenant-inferd serve is not wired yet: bind an OpenAI-compatible engine on \
                 {listen}, then relay it with `covenant-inferd tunnel --target {listen}`"
            )
        }
    }
}
