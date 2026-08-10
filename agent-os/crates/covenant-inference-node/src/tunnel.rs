use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use covenant_inference_protocol::{node_id, TunnelRegistration, UnsignedTunnelRegistration};
use ed25519_dalek::SigningKey;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_rustls::TlsConnector;

use crate::backoff::Backoff;
use crate::frame::{read_frame, write_frame};
use crate::target::InferenceTarget;

/// A connection that stays up at least this long is treated as healthy: its
/// eventual close clears the reconnect backoff instead of escalating it. Shorter
/// sessions look like a gateway that keeps refusing us and are backed off.
const HEALTHY_SESSION: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct TunnelConfig {
    /// `host:port` of the gateway the node dials outbound.
    pub gateway: String,
    /// TLS server name presented on that dial; must match the gateway cert.
    pub server_name: String,
    pub ca_certificate: PathBuf,
    pub client_certificate: PathBuf,
    pub client_key: PathBuf,
    /// Opaque handle the gateway uses to route relays to this node's slots.
    pub connection_id: String,
    pub slots: u16,
}

/// The single service an inference node relays. A node only ever forwards to its
/// local model server, but the service is kept as a tagged frame so the gateway
/// wire format stays open to another service later without a protocol break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayService {
    Inference,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RelayOpen {
    pub service: RelayService,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayReady {
    pub ready: bool,
    pub error: Option<String>,
}

/// Runs the outbound tunnel pool: `slots` workers, each holding one reverse
/// connection open to the gateway, until Ctrl-C or a worker panics.
pub async fn run<T>(config: TunnelConfig, target: T, signing_key: SigningKey) -> anyhow::Result<()>
where
    T: InferenceTarget,
{
    validate_identifier(&config.connection_id)?;
    if config.slots == 0 || config.slots > 64 {
        anyhow::bail!("tunnel slots must be between 1 and 64");
    }
    install_crypto_provider();
    let tls = tls_connector(
        &config.ca_certificate,
        &config.client_certificate,
        &config.client_key,
    )?;
    let config = Arc::new(config);
    let signing_key = Arc::new(signing_key);
    let target = Arc::new(target);

    let mut workers = JoinSet::new();
    for _ in 0..config.slots {
        workers.spawn(worker(
            config.clone(),
            tls.clone(),
            signing_key.clone(),
            target.clone(),
        ));
    }

    tokio::select! {
        result = workers.join_next() => match result {
            Some(Ok(())) => anyhow::bail!("a tunnel worker exited unexpectedly"),
            Some(Err(error)) => Err(error.into()),
            None => anyhow::bail!("no tunnel workers were started"),
        },
        signal = tokio::signal::ctrl_c() => {
            signal.context("install tunnel shutdown signal")?;
            workers.abort_all();
            Ok(())
        }
    }
}

async fn worker<T>(
    config: Arc<TunnelConfig>,
    tls: TlsConnector,
    signing_key: Arc<SigningKey>,
    target: Arc<T>,
) where
    T: InferenceTarget,
{
    let backoff = Backoff::default();
    let mut attempt = 0_u32;
    loop {
        let started = Instant::now();
        if let Err(error) = connect_and_serve(&config, &tls, &signing_key, target.as_ref()).await {
            tracing::warn!(%error, slot = %config.connection_id, "outbound tunnel slot disconnected");
        }
        // A session that lasted clears the backoff; a rapid failure escalates it.
        attempt = if started.elapsed() >= HEALTHY_SESSION {
            0
        } else {
            attempt.saturating_add(1)
        };
        sleep(backoff.sample(attempt, os_entropy())).await;
    }
}

async fn connect_and_serve<T>(
    config: &TunnelConfig,
    tls: &TlsConnector,
    signing_key: &SigningKey,
    target: &T,
) -> anyhow::Result<()>
where
    T: InferenceTarget,
{
    let tcp = TcpStream::connect(&config.gateway)
        .await
        .with_context(|| format!("connect tunnel gateway {}", config.gateway))?;
    tcp.set_nodelay(true)?;
    let server_name = ServerName::try_from(config.server_name.clone())
        .context("tunnel server name is invalid")?;
    let mut stream = tls
        .connect(server_name, tcp)
        .await
        .context("establish tunnel mTLS")?;
    let registration = signed_registration(&config.connection_id, signing_key)?;
    serve_connection(&mut stream, &registration, target).await
}

/// Builds the signed handshake the node presents to the gateway. `issued_at` is
/// stamped now so the gateway's freshness check passes; a fresh one is minted for
/// every dial.
pub fn signed_registration(
    connection_id: &str,
    key: &SigningKey,
) -> anyhow::Result<TunnelRegistration> {
    Ok(TunnelRegistration::sign(
        UnsignedTunnelRegistration {
            node_id: node_id(&key.verifying_key()),
            device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            connection_id: connection_id.to_owned(),
            issued_at: Utc::now(),
        },
        key,
    )?)
}

/// The node side of one relay: announce who we are, wait for the gateway to ask
/// for a stream, dial the local inference target and splice the two together.
///
/// Generic over the transport so the frame + relay path can be exercised over an
/// in-process pipe in tests without standing up real mTLS; in production `S` is a
/// [`tokio_rustls`] mTLS stream.
pub async fn serve_connection<S, T>(
    stream: &mut S,
    registration: &TunnelRegistration,
    target: &T,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    T: InferenceTarget,
{
    write_frame(stream, registration).await?;
    // The gateway announces which service it wants. A node serves exactly one, so
    // decoding is the validation: an unknown service fails to deserialize and the
    // connection is dropped.
    let _open: RelayOpen = read_frame(stream).await?;
    let mut local = match target.connect().await {
        Ok(local) => local,
        Err(error) => {
            write_frame(
                stream,
                &RelayReady {
                    ready: false,
                    error: Some("inference_target_unavailable".to_owned()),
                },
            )
            .await?;
            return Err(error);
        }
    };
    write_frame(
        stream,
        &RelayReady {
            ready: true,
            error: None,
        },
    )
    .await?;
    tokio::io::copy_bidirectional(stream, &mut local)
        .await
        .context("relay inference stream")?;
    Ok(())
}

fn install_crypto_provider() {
    // Idempotent: the process may already have installed a provider (e.g. the
    // binary does so at startup). Ignoring the error keeps repeat calls harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn tls_connector(ca: &Path, certificate: &Path, key: &Path) -> anyhow::Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(ca)? {
        roots
            .add(certificate)
            .context("add tunnel CA certificate")?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates(certificate)?, private_key(key)?)
        .context("configure tunnel client authentication")?;
    Ok(TlsConnector::from(Arc::new(config)))
}

fn certificates(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)
        .with_context(|| format!("open certificate {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .context("parse PEM certificates")
}

fn private_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path)
        .with_context(|| format!("parse private key {}", path.display()))
}

fn os_entropy() -> u64 {
    let mut bytes = [0_u8; 8];
    getrandom::getrandom(&mut bytes).expect("os rng");
    u64::from_le_bytes(bytes)
}

fn validate_identifier(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("connection identifier is invalid");
    }
    Ok(())
}
