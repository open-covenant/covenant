use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use covenant_inference_node::frame::read_frame;
use covenant_inference_protocol::{node_id, verifying_key, TunnelRegistration};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

use crate::pool::TunnelPool;
use crate::registry::ISSUED_AT_SKEW_SECONDS;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the mTLS acceptor for the node-facing tunnel: the gateway presents its
/// server certificate and requires every node to present a client certificate that
/// chains to `client_ca`. The signed [`TunnelRegistration`] read afterwards binds
/// that TLS identity to a node id; the certificate only has to be trusted by the CA.
pub fn tls_acceptor(cert: &Path, key: &Path, client_ca: &Path) -> anyhow::Result<TlsAcceptor> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(client_ca)? {
        roots.add(certificate).context("add tunnel client CA")?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .context("build tunnel client verifier")?;
    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates(cert)?, private_key(key)?)
        .context("configure tunnel server certificate")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Accepts node tunnels forever, parking each verified connection in the pool. One
/// task per connection: a slow or hostile handshake can't stall the accept loop.
pub async fn serve(
    listen: SocketAddr,
    acceptor: TlsAcceptor,
    pool: TunnelPool,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind tunnel listener {listen}"))?;
    tracing::info!(%listen, "mTLS node tunnel listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let acceptor = acceptor.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            if let Err(error) = accept(stream, &acceptor, &pool).await {
                tracing::warn!(%peer, %error, "node tunnel registration failed");
            }
        });
    }
}

async fn accept(
    stream: TcpStream,
    acceptor: &TlsAcceptor,
    pool: &TunnelPool,
) -> anyhow::Result<()> {
    let mut tls = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
        .await
        .context("node mTLS handshake timed out")??;
    let node = register(&mut tls).await?;
    pool.insert(node.clone(), Box::new(tls))
        .map_err(|_| anyhow::anyhow!("tunnel pool is full"))?;
    tracing::info!(node_id = %node, "parked reverse tunnel");
    Ok(())
}

/// Reads and verifies a node's signed tunnel registration off an established stream,
/// returning the node id the connection may serve. Generic over the transport so the
/// exact registration path runs over an in-process duplex pipe in tests, no TLS.
pub async fn register<S>(stream: &mut S) -> anyhow::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let registration: TunnelRegistration = timeout(HANDSHAKE_TIMEOUT, read_frame(stream))
        .await
        .context("node registration timed out")??;
    validate(&registration)?;
    Ok(registration.node_id)
}

fn validate(registration: &TunnelRegistration) -> anyhow::Result<()> {
    let key = verifying_key(&registration.device_public_key).context("registration device key")?;
    if node_id(&key) != registration.node_id {
        anyhow::bail!("tunnel identity does not match its device key");
    }
    let skew = registration
        .issued_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .abs();
    if skew > ISSUED_AT_SKEW_SECONDS {
        anyhow::bail!("tunnel registration timestamp is stale");
    }
    registration
        .verify(&key)
        .context("tunnel registration signature")
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use covenant_inference_protocol::{TunnelRegistration, UnsignedTunnelRegistration};
    use ed25519_dalek::SigningKey;

    fn signed(
        key: &SigningKey,
        mutate: impl FnOnce(&mut TunnelRegistration),
    ) -> TunnelRegistration {
        let mut registration = TunnelRegistration::sign(
            UnsignedTunnelRegistration {
                node_id: node_id(&key.verifying_key()),
                device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
                connection_id: "conn-1".into(),
                issued_at: Utc::now(),
            },
            key,
        )
        .unwrap();
        mutate(&mut registration);
        registration
    }

    #[test]
    fn accepts_a_fresh_matching_registration() {
        let key = SigningKey::from_bytes(&[3_u8; 32]);
        assert!(validate(&signed(&key, |_| {})).is_ok());
    }

    #[test]
    fn rejects_an_identity_that_does_not_match_the_key() {
        let key = SigningKey::from_bytes(&[3_u8; 32]);
        let forged = signed(&key, |registration| {
            registration.node_id = "0xdeadbeef".into();
        });
        assert!(validate(&forged).is_err());
    }

    #[test]
    fn rejects_a_stale_registration() {
        let key = SigningKey::from_bytes(&[3_u8; 32]);
        let stale = signed(&key, |registration| {
            registration.issued_at -= chrono::Duration::seconds(ISSUED_AT_SKEW_SECONDS + 60);
        });
        assert!(validate(&stale).is_err());
    }
}
