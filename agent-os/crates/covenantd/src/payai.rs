//! Daemon glue for the PayAI trust surface. `covenant-payai` owns the indexer,
//! scoring, and credential envelope; the daemon holds only config. No funds,
//! no `solana-sdk`.

pub use covenant_payai::PayAiConfig;

/// Config only; the indexer is stateless and built per call. Mirrors
/// `MetaplexState`/`HyreState` so the daemon wires it the same way.
pub struct PayAiState {
    pub config: PayAiConfig,
}

impl PayAiState {
    pub fn new(config: PayAiConfig) -> Self {
        Self { config }
    }
}
