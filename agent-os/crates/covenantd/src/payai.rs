//! Daemon glue for the PayAI trust surface.
//!
//! `covenant-payai` owns the settlement indexer, reputation scoring, and the
//! signed-credential envelope. The daemon holds only the config here; the
//! reputation read is computed on demand from PayAI's public Solana
//! settlements and signed with the daemon identity. No funds, no `solana-sdk`.

pub use covenant_payai::PayAiConfig;

/// Materialised PayAI profile: just config today (the indexer is stateless and
/// built per call). Mirrors the `MetaplexState`/`HyreState` shape so the daemon
/// wires it the same way.
pub struct PayAiState {
    pub config: PayAiConfig,
}

impl PayAiState {
    pub fn new(config: PayAiConfig) -> Self {
        Self { config }
    }
}
