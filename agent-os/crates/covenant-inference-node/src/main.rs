use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use covenant_inference_node::daemon::{self, EnrollArgs, HeartbeatArgs};
use covenant_inference_node::serve::{self, ServeConfig, SpawnConfig};
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
    /// Front a local llama.cpp engine with the node's OpenAI-compatible surface and
    /// the served model's identity. Put it behind the gateway by pointing the tunnel
    /// at it: `covenant-inferd tunnel --target <listen>`.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: SocketAddr,
        /// Base URL of an already-running engine to proxy. Ignored with `--spawn`.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        engine_url: String,
        /// Model id this node advertises, e.g. `qwen3:8b`.
        #[arg(long)]
        model: String,
        /// GGUF weights file; hashed into the model identity, and loaded by `--spawn`.
        #[arg(long)]
        weights: PathBuf,
        #[arg(long, default_value = "q4_k_m")]
        quantization: String,
        #[arg(long, default_value = "llama.cpp")]
        runtime: String,
        /// Pin the engine build in the identity; detected from the binary otherwise.
        #[arg(long)]
        runtime_version: Option<String>,
        /// The verifiable path is greedy: temperature 0 by default.
        #[arg(long, default_value_t = 0.0)]
        temperature: f64,
        #[arg(long, default_value_t = 1.0)]
        top_p: f64,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long, default_value_t = 512)]
        sampling_max_tokens: u32,
        /// Spawn and supervise `llama-server` instead of proxying an external one.
        #[arg(long, default_value_t = false)]
        spawn: bool,
        #[arg(long, default_value = "llama-server")]
        engine_bin: PathBuf,
        #[arg(long, default_value_t = 8080)]
        engine_port: u16,
        #[arg(long, default_value_t = 4096)]
        context_size: u32,
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
        Command::Serve {
            listen,
            engine_url,
            model,
            weights,
            quantization,
            runtime,
            runtime_version,
            temperature,
            top_p,
            seed,
            sampling_max_tokens,
            spawn,
            engine_bin,
            engine_port,
            context_size,
        } => {
            serve::run(ServeConfig {
                listen,
                engine_url,
                model,
                weights,
                quantization,
                runtime,
                runtime_version,
                sampling: SamplingParams {
                    temperature,
                    top_p,
                    seed,
                    max_tokens: sampling_max_tokens,
                },
                spawn: spawn.then_some(SpawnConfig {
                    engine_bin,
                    engine_port,
                    context_size,
                }),
            })
            .await
        }
    }
}
