//! WURK agent-to-human provider profile for Covenant.
//!
//! WURK (wurk.fun) is an x402 marketplace. This crate wires ONLY its
//! agent-to-human microjob family: create a human-review job, read the
//! submissions, and pick winners. The social-engagement/vote endpoints
//! are never reachable through here (see [`config::ALLOWED_PATHS`]).
//!
//! Payment reuses covenant-x402's [`covenant_x402::Signer`] (the
//! funding-key sidecar) and [`covenant_x402::PaymentRequirements`].
//! WURK's 402 is the object/`accepts` shape with `amount` +
//! `extra.feePayer`, but the payment header is **`PAYMENT-SIGNATURE`**
//! (not x402's `x-payment`), so this crate runs its own 402-then-pay
//! loop in [`x402`]. View and winner-selection are free (secret-authed)
//! and need no signer.

pub mod config;
pub mod tools;
pub mod view;
pub mod x402;

pub use config::WurkConfig;
pub use tools::{wurk_specs, wurk_tool, wurk_tools, PaidRequest, PaidResponse, WurkExecutor};
pub use view::{parse_view, JobView, Submission};
pub use x402::{execute_paid, parse_challenge, PaidHttp};

/// Errors raised across the WURK profile.
#[derive(Debug, thiserror::Error)]
pub enum WurkError {
    #[error("challenge: {0}")]
    Challenge(String),
    #[error("not allowed: {0}")]
    NotAllowed(String),
    #[error("execute: {0}")]
    Execute(String),
    #[error("view: {0}")]
    View(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, WurkError>;
