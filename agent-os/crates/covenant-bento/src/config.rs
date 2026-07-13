//! Bento provider configuration.
//!
//! Three switches, all off by default. `enabled` is the master: nothing
//! registers without it. `protect_enabled` turns on live screening through
//! the guard sidecar. `program_id` being set is what un-gates the on-chain
//! reputation read, and only Bento can supply a real one along with the
//! account layout.
//!
//! The sidecar's environment carries only non-secret paths (a keypair file
//! path and `PATH`), never the key itself, so this struct is safe to
//! serialize and debug.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default Solana RPC for on-chain reads. Devnet, because that is where a
/// first Bento integration registers and runs; override for mainnet.
pub const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";

/// Bento's deployed program id on devnet, read from the live relayer
/// (`GET api.bentoguard.xyz/api/v1/system/relayer`). Set `program_id` to this
/// to register the reputation tool. The on-chain read still needs the agent
/// account layout, which lives on Bento's MagicBlock ER, not base devnet (see
/// the reader).
pub const DEVNET_PROGRAM_ID: &str = "A5vQdPeJH2Yn72RmXHyrFjErUTqPwX83e6of4LBchEbG";

/// Default hard deadline for a single screen, in milliseconds. A live
/// `protect()` runs an on-chain action flow plus AI scoring and takes tens of
/// seconds (the SDK's own default is 120s), so this matches it rather than
/// killing legitimate verdicts. The sidecar gets it as its inner timeout; the
/// Rust side kills the child a few seconds later as a backstop.
pub const DEFAULT_GUARD_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BentoConfig {
    /// Master switch. `false` registers no tools and makes no calls.
    #[serde(default)]
    pub enabled: bool,
    /// Solana RPC used by the on-chain reputation reader.
    #[serde(default = "default_rpc_url")]
    pub rpc_url: String,
    /// Bento's on-chain program id. `Some` un-gates the reputation reader;
    /// `None` (the default) leaves it off, because reads also need the PDA
    /// seeds and account layout, which ship together with the program id.
    #[serde(default)]
    pub program_id: Option<String>,
    /// Live screening gate. When `false` (the default) the `bento.screen`
    /// tool is not registered even if a guard binary is configured.
    #[serde(default)]
    pub protect_enabled: bool,
    /// Path to the guard sidecar executable (a Node runner over Bento's SDK).
    pub guard_binary: Option<PathBuf>,
    /// Environment handed to the guard sidecar. Carries only non-secret
    /// values (`PATH`, and a keypair file path the sidecar reads); the key
    /// itself never lands here.
    #[serde(default)]
    pub guard_env: Vec<(String, String)>,
    /// Hard deadline for a single screen, in milliseconds.
    #[serde(default = "default_guard_timeout_ms")]
    pub guard_timeout_ms: u64,
}

fn default_rpc_url() -> String {
    DEFAULT_RPC_URL.to_string()
}

fn default_guard_timeout_ms() -> u64 {
    DEFAULT_GUARD_TIMEOUT_MS
}

impl Default for BentoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rpc_url: default_rpc_url(),
            program_id: None,
            protect_enabled: false,
            guard_binary: None,
            guard_env: Vec::new(),
            guard_timeout_ms: DEFAULT_GUARD_TIMEOUT_MS,
        }
    }
}

impl BentoConfig {
    /// Whether the on-chain reputation reader should be active: master on and
    /// a program id supplied.
    pub fn reputation_ready(&self) -> bool {
        self.enabled && self.program_id.is_some()
    }

    /// Whether live screening should be active: master on, the gate set, and
    /// a guard binary to spawn.
    pub fn screening_ready(&self) -> bool {
        self.enabled && self.protect_enabled && self.guard_binary.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_fully_off_on_devnet() {
        let c = BentoConfig::default();
        assert!(!c.enabled);
        assert!(!c.protect_enabled);
        assert!(c.program_id.is_none());
        assert_eq!(c.rpc_url, DEFAULT_RPC_URL);
        assert_eq!(c.guard_timeout_ms, DEFAULT_GUARD_TIMEOUT_MS);
        assert!(!c.reputation_ready());
        assert!(!c.screening_ready());
    }

    #[test]
    fn master_off_keeps_everything_off() {
        let c = BentoConfig {
            enabled: false,
            program_id: Some("Bento1111111111111111111111111111111111111".into()),
            protect_enabled: true,
            guard_binary: Some("/x".into()),
            ..Default::default()
        };
        assert!(!c.reputation_ready(), "master off must gate reputation");
        assert!(!c.screening_ready(), "master off must gate screening");
    }

    #[test]
    fn readiness_needs_its_own_inputs() {
        let reads = BentoConfig {
            enabled: true,
            program_id: Some("Bento1111111111111111111111111111111111111".into()),
            ..Default::default()
        };
        assert!(reads.reputation_ready());
        assert!(!reads.screening_ready(), "no guard binary, no screening");

        let screen = BentoConfig {
            enabled: true,
            protect_enabled: true,
            guard_binary: Some("/usr/local/bin/bento-guard".into()),
            ..Default::default()
        };
        assert!(screen.screening_ready());
        assert!(!screen.reputation_ready(), "no program id, no reads");
    }
}
