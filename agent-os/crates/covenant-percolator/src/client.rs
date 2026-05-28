//! Percolator client seam. The trait abstracts the on-chain reads and
//! writes the keeper needs; the in-memory [`MockPercolator`] is the
//! testable mirror of the program. A real `solana`-feature-gated
//! implementation lands once percolator-prog's IDL is in-tree.

use async_trait::async_trait;
use std::sync::Mutex;

use crate::state::{KeeperAction, MarketState};

#[async_trait]
pub trait PercolatorClient: Send + Sync {
    /// Snapshot of the current market — what the policy reads to
    /// decide actions.
    async fn read_market(&self, market_address: &str) -> Result<MarketState, ClientError>;

    /// Submit one keeper action. The signature is `None` in dry-run /
    /// mock execution; the real client returns the on-chain tx sig.
    async fn execute(
        &self,
        market_address: &str,
        action: &KeeperAction,
    ) -> Result<Execution, ClientError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    pub tx_signature: Option<String>,
    /// Slot at which the action was simulated / submitted. The mock
    /// uses its internal clock; the real client carries the cluster slot.
    pub slot: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("market {0} not found")]
    NotFound(String),
    #[error("invalid action: {0}")]
    Invalid(String),
    #[error("client: {0}")]
    Other(String),
}

/// In-memory mock used for unit-testing the keeper loop and the
/// capability/budget gates without touching a live cluster. Push-mark
/// and crank actions mutate the toy state so the loop sees the effect
/// on subsequent ticks; the recovery trio is recorded only, since its
/// real semantics live on-chain.
pub struct MockPercolator {
    state: Mutex<MarketState>,
    log: Mutex<Vec<KeeperAction>>,
}

impl MockPercolator {
    pub fn new(state: MarketState) -> Self {
        Self {
            state: Mutex::new(state),
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn executed(&self) -> Vec<KeeperAction> {
        self.log.lock().unwrap().clone()
    }

    /// Bump the mock's clock so a follow-up tick sees the new slot.
    pub fn advance_slot(&self, to: u64) {
        let mut s = self.state.lock().unwrap();
        if to > s.current_slot {
            s.current_slot = to;
        }
    }
}

#[async_trait]
impl PercolatorClient for MockPercolator {
    async fn read_market(&self, market_address: &str) -> Result<MarketState, ClientError> {
        let s = self.state.lock().unwrap();
        if s.market_address != market_address {
            return Err(ClientError::NotFound(market_address.to_string()));
        }
        Ok(s.clone())
    }

    async fn execute(
        &self,
        market_address: &str,
        action: &KeeperAction,
    ) -> Result<Execution, ClientError> {
        let mut s = self.state.lock().unwrap();
        if s.market_address != market_address {
            return Err(ClientError::NotFound(market_address.to_string()));
        }
        let slot = s.current_slot;
        match action {
            KeeperAction::PushHyperpMark {
                asset_index,
                mark_e6,
            } => {
                let asset = s
                    .assets
                    .iter_mut()
                    .find(|a| a.index == *asset_index)
                    .ok_or_else(|| ClientError::Invalid(format!("unknown asset {asset_index}")))?;
                asset.last_mark_slot = slot;
                asset.last_mark_e6 = *mark_e6;
            }
            KeeperAction::Crank => {
                s.last_crank_slot = slot;
            }
            KeeperAction::RecoveryForfeitLeg { .. }
            | KeeperAction::RecoveryRebalanceReduce { .. }
            | KeeperAction::RecoveryFinalize { .. } => {
                // Real semantics live in percolator-prog; the mock only
                // records the action so tests can assert it ran.
            }
        }
        self.log.lock().unwrap().push(action.clone());
        Ok(Execution {
            tx_signature: None,
            slot,
        })
    }
}
