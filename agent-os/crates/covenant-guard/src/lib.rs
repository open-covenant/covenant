//! covguard: let a coding agent run unattended.
//!
//! The guard runs as the parent process, outside the sandbox the agent lives
//! in. It holds three things the agent cannot reach around:
//!
//!   * a metering proxy that every model call is routed through, so spend is
//!     counted and capped from outside the agent's process (`proxy`, `ledger`);
//!   * an OS sandbox that confines writes to the workspace and blocks every
//!     network path except the loopback proxy (`sandbox`);
//!   * a hash-chained, signed record of the run (`chain`, `receipt`).
//!
//! `run` wires them together; `cli` parses the command line.

pub mod chain;
pub mod cli;
pub mod ledger;
pub mod mcp;
pub mod pricing;
pub mod proxy;
pub mod receipt;
pub mod run;
pub mod sandbox;

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch. One place so the whole crate agrees on
/// the clock, and tests can reason about it.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// State directory for keys and receipts, `~/.covenant-guard` unless
/// `COVGUARD_HOME` overrides it.
pub fn home_dir() -> std::path::PathBuf {
    if let Ok(h) = std::env::var("COVGUARD_HOME") {
        return std::path::PathBuf::from(h);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join(".covenant-guard")
}
