use std::sync::Mutex;

use async_trait::async_trait;

use crate::Result;

/// A single settling transfer, taken verbatim from the 402 quote: `amount_raw` base units
/// of `mint` (owned by `token_program`, asserting `decimals`) to `recipient`. Circuit's 402
/// quotes CIRC by default and lists alternates — including $CVNT — under `acceptedTokens`,
/// so the mint is not fixed; the engine picks one and hands the payer exactly what to send.
#[derive(Debug, Clone)]
pub struct TokenTransfer<'a> {
    pub mint: &'a str,
    pub token_program: &'a str,
    pub decimals: u8,
    pub recipient: &'a str,
    pub amount_raw: u64,
}

/// The only thing the x402 loop needs from a wallet: settle a [`TokenTransfer`] on Solana
/// and return the confirmed transaction signature. This mirrors the Circuit SDK's paying
/// wallet, so a Covenant-paid call settles byte-for-byte like a first-party one — a real
/// Token-2022 transfer into Circuit's treasury, which auto-settles to CIRC on their side
/// when we pay in $CVNT.
///
/// The real implementation lives behind the crate's `solana` feature (or a signing
/// sidecar) so a funding key never enters this library.
#[async_trait]
pub trait CircPayer: Send + Sync {
    /// Settle the transfer and return the confirmed transaction signature.
    async fn pay(&self, transfer: &TokenTransfer<'_>) -> Result<String>;

    /// Payer address, for logging and audit. `None` when read-only.
    fn address(&self) -> Option<&str> {
        None
    }
}

/// A payer that never touches the chain: it returns a deterministic fake signature and
/// records what it was asked to pay. Used by the wiremock suite to exercise the full
/// pay-and-retry loop and capability enforcement without settling money, and by the
/// example as a dry run.
pub struct MockCircPayer {
    address: String,
    paid: Mutex<Vec<(String, u64)>>,
}

impl MockCircPayer {
    pub fn new() -> Self {
        Self {
            address: "MockCircPayer1111111111111111111111111111111".into(),
            paid: Mutex::new(Vec::new()),
        }
    }

    /// Every `(recipient, amount_raw)` this payer was asked to settle, in order.
    pub fn payments(&self) -> Vec<(String, u64)> {
        self.paid.lock().expect("payer lock").clone()
    }

    /// Total raw CIRC this payer was asked to settle.
    pub fn total_paid(&self) -> u64 {
        self.paid
            .lock()
            .expect("payer lock")
            .iter()
            .map(|(_, a)| *a)
            .sum()
    }
}

impl Default for MockCircPayer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CircPayer for MockCircPayer {
    async fn pay(&self, transfer: &TokenTransfer<'_>) -> Result<String> {
        self.paid
            .lock()
            .expect("payer lock")
            .push((transfer.recipient.to_string(), transfer.amount_raw));
        // A shape-plausible but obviously-fake signature so a real settle is never mistaken for this.
        Ok(format!(
            "mock-circ-tx-{}-to-{}",
            transfer.amount_raw,
            &transfer.recipient[..transfer.recipient.len().min(8)]
        ))
    }

    fn address(&self) -> Option<&str> {
        Some(&self.address)
    }
}
