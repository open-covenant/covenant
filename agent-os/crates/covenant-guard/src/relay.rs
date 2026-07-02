//! Loopback bridge for the Linux sandbox.
//!
//! On Linux the agent runs in its own network namespace (`bwrap --unshare-net`),
//! which cuts every route including loopback to the host's metering proxy. So
//! the agent's only path out is relayed through a unix socket bind-mounted into
//! the sandbox: inside the namespace `run_relay` listens on the proxy's port and
//! forwards to that socket; on the host `run_bridge` accepts the socket and
//! forwards to the real proxy. External addresses have no route at all, so the
//! meter can't be tunneled past.

use std::path::Path;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};

/// Host side: accept on the unix socket, forward to the proxy on loopback. Runs
/// in the covguard process for the life of the run.
pub async fn run_bridge(sock: &Path, proxy_port: u16) -> std::io::Result<()> {
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock)?;
    loop {
        let (mut inbound, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", proxy_port)).await {
                let _ = copy_bidirectional(&mut inbound, &mut upstream).await;
            }
        });
    }
}

/// Sandbox side: accept on `tcp_listen` (the proxy's port, inside the isolated
/// namespace), forward to the host's unix socket. Runs as `covguard __relay`.
pub async fn run_relay(tcp_listen: &str, sock: &Path) -> std::io::Result<()> {
    let listener = TcpListener::bind(tcp_listen).await?;
    let sock = sock.to_path_buf();
    loop {
        let (mut inbound, _) = listener.accept().await?;
        let sock = sock.clone();
        tokio::spawn(async move {
            if let Ok(mut upstream) = UnixStream::connect(&sock).await {
                let _ = copy_bidirectional(&mut inbound, &mut upstream).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // End to end: a TCP client → relay (tcp) → unix socket → bridge → a mock
    // upstream TCP server. This is the exact two-hop path the Linux sandbox uses,
    // and unix sockets work on macOS too, so it runs anywhere.
    #[tokio::test]
    async fn relay_and_bridge_move_bytes_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("bridge.sock");

        // Mock upstream: echo a fixed reply and read the request.
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
            s.write_all(b"world").await.unwrap();
        });

        // Bridge: unix sock → upstream.
        let bsock = sock.clone();
        tokio::spawn(async move {
            let _ = run_bridge(&bsock, upstream_port).await;
        });
        // Relay: a fresh TCP port → unix sock.
        let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_port = relay.local_addr().unwrap().port();
        drop(relay); // free the port for run_relay to bind
        let rsock = sock.clone();
        tokio::spawn(async move {
            let _ = run_relay(&format!("127.0.0.1:{relay_port}"), &rsock).await;
        });

        // Give the listeners a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let mut client = TcpStream::connect(("127.0.0.1", relay_port)).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"world");
    }
}
