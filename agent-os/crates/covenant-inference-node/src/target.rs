use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// The seam between the tunnel and whatever actually serves tokens. The transport
/// only needs to open a byte stream to the local engine and ask whether it's
/// alive; it knows nothing about HTTP, prompts or model formats.
///
/// `covenant-inferd serve` will front an OpenAI-compatible server (llama.cpp,
/// vLLM, ...) and expose it here as a [`LocalHttpTarget`] pointing at that
/// server's listen address. Wiring in that engine is out of scope for the node
/// daemon - implementing this trait is the entire contract it has to satisfy.
pub trait InferenceTarget: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    fn connect(&self) -> impl Future<Output = anyhow::Result<Self::Stream>> + Send;
    fn health(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Forwards relayed connections to a local TCP listener - the address an
/// OpenAI-compatible inference server is bound to on the node.
#[derive(Debug, Clone, Copy)]
pub struct LocalHttpTarget {
    addr: SocketAddr,
}

impl LocalHttpTarget {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl InferenceTarget for LocalHttpTarget {
    type Stream = TcpStream;

    async fn connect(&self) -> anyhow::Result<TcpStream> {
        let stream = TcpStream::connect(self.addr)
            .await
            .with_context(|| format!("connect inference target {}", self.addr))?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }

    async fn health(&self) -> anyhow::Result<()> {
        // A live socket is all the transport needs to decide the slot is worth
        // advertising. Once `serve` fronts a real engine, a GET /health against
        // the OpenAI-compatible surface belongs right here.
        let probe = timeout(HEALTH_TIMEOUT, TcpStream::connect(self.addr))
            .await
            .with_context(|| format!("inference target {} health probe timed out", self.addr))?
            .with_context(|| format!("inference target {} is unreachable", self.addr))?;
        drop(probe);
        Ok(())
    }
}
