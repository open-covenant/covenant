//! Invoica provider for Covenant.
//!
//! Invoica is the money and compliance layer for agents (invoicing,
//! settlement, tax). Covenant is the authority and accountability layer. They
//! barely overlap: settlement stays on Invoica's rail, and Covenant scopes,
//! brokers, and audits the call around it.
//!
//! Phase 1 wraps Invoica's Bearer-key invoice API as Covenant MCP tools:
//! create, get, and list invoices, and read settlement state off the invoice.
//! The daemon holds the Invoica API key inside the [`InvoicaClient`] and
//! injects it per request, so the agent reaches the tool through the daemon and
//! never sees the key.
//! Every tool sits behind a `tool.call.*` capability and lands on the audit
//! chain through the daemon's generic tool path.
//!
//! Invoica's x402-paid surface (the priced invoice/settle/tax endpoints at
//! `/api/x402/*`), the provenance envelope binding a task to its settlement
//! signature and audit trail, and the live x402 payment path are Phase 2. This
//! crate stays on the Bearer-key REST surface.
//!
//! Lane discipline: Covenant never settles or runs the tax engine itself
//! (Invoica owns that), and Invoica never issues capability grants or holds the
//! audit chain (Covenant owns that).

#![deny(unsafe_code)]

pub mod client;
pub mod config;
pub mod tools;
pub mod types;

pub use client::InvoicaClient;
pub use config::{InvoicaConfig, DEFAULT_BASE_URL, DEFAULT_CHAIN};
pub use tools::{invoica_tools, PROVIDER};
pub use types::CreateInvoiceRequest;

/// Errors surfaced by the Invoica provider.
#[derive(Debug, thiserror::Error)]
pub enum InvoicaError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// A non-2xx response, with Invoica's machine `code` when it sends one.
    #[error("invoica api [{}]: {}{}", status, message,
            code.as_deref().map(|c| format!(" [{c}]")).unwrap_or_default())]
    Api {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("decode: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, InvoicaError>;
