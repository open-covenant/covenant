use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};

/// A parked reverse connection is discarded once it has sat unused this long. A
/// node redials on a fixed cadence, so a tunnel older than this is almost always a
/// half-dead socket the node has already replaced; taking it would just fail a
/// request that another connection could have served.
pub const TUNNEL_MAX_AGE: Duration = Duration::from_secs(90);

/// Ceilings so a misbehaving or spoofed node can't park unbounded idle sockets and
/// exhaust the gateway's file descriptors.
const MAX_IDLE_TOTAL: usize = 1_024;
const MAX_IDLE_PER_NODE: usize = 64;

/// Any byte stream the pool can hold and the proxy can run HTTP/1 over: a
/// `tokio-rustls` mTLS stream in production, an in-process duplex pipe in tests.
pub trait TunnelStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> TunnelStream for T {}

pub type PooledStream = Box<dyn TunnelStream>;

struct Idle {
    inserted_at: Instant,
    stream: PooledStream,
}

/// Reverse tunnels keyed by node id. Each node parks several connections (one per
/// tunnel slot); routing pops one per request and the node redials to refill.
#[derive(Clone, Default)]
pub struct TunnelPool {
    inner: Arc<Mutex<HashMap<String, VecDeque<Idle>>>>,
}

#[derive(Debug)]
pub struct PoolFull;

impl TunnelPool {
    pub fn insert(&self, node_id: String, stream: PooledStream) -> Result<(), PoolFull> {
        let mut tunnels = self.lock();
        let total: usize = tunnels.values().map(VecDeque::len).sum();
        let queue = tunnels.entry(node_id).or_default();
        if total >= MAX_IDLE_TOTAL || queue.len() >= MAX_IDLE_PER_NODE {
            return Err(PoolFull);
        }
        queue.push_back(Idle {
            inserted_at: Instant::now(),
            stream,
        });
        Ok(())
    }

    /// Pops the oldest live connection for a node, dropping any that have aged out.
    pub fn take(&self, node_id: &str) -> Option<PooledStream> {
        let mut tunnels = self.lock();
        let queue = tunnels.get_mut(node_id)?;
        while let Some(idle) = queue.pop_front() {
            if idle.inserted_at.elapsed() <= TUNNEL_MAX_AGE {
                return Some(idle.stream);
            }
        }
        tunnels.remove(node_id);
        None
    }

    /// Whether a node has at least one connection young enough to be worth routing
    /// to. Prunes aged-out sockets it walks past so `take` and `has` agree.
    pub fn has(&self, node_id: &str) -> bool {
        let mut tunnels = self.lock();
        let Some(queue) = tunnels.get_mut(node_id) else {
            return false;
        };
        while queue
            .front()
            .is_some_and(|idle| idle.inserted_at.elapsed() > TUNNEL_MAX_AGE)
        {
            queue.pop_front();
        }
        if queue.is_empty() {
            tunnels.remove(node_id);
            return false;
        }
        true
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, VecDeque<Idle>>> {
        self.inner.lock().expect("tunnel pool mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn stream() -> PooledStream {
        Box::new(duplex(64).0)
    }

    #[test]
    fn take_returns_parked_connections_fifo_then_empties() {
        let pool = TunnelPool::default();
        assert!(!pool.has("node"));
        pool.insert("node".into(), stream()).unwrap();
        pool.insert("node".into(), stream()).unwrap();
        assert!(pool.has("node"));
        assert!(pool.take("node").is_some());
        assert!(pool.take("node").is_some());
        assert!(pool.take("node").is_none());
        assert!(!pool.has("node"));
    }

    #[test]
    fn per_node_capacity_is_enforced() {
        let pool = TunnelPool::default();
        for _ in 0..MAX_IDLE_PER_NODE {
            pool.insert("node".into(), stream()).unwrap();
        }
        assert!(pool.insert("node".into(), stream()).is_err());
    }
}
