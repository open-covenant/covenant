//! Reference client for the client-sealed vault. Against a coordinator
//! serving the vault, it seals a secret under a key it generates, stores
//! the ciphertext, lists it, fetches and opens it back, and deletes it —
//! the whole lifecycle a buyer runs, through the same buyer-library calls
//! an application would use.
//!
//! The coordinator only ever holds the ciphertext. The key printed below
//! stays on this side; lose it and the stored secret is unrecoverable,
//! which is the guarantee, not a bug.
//!
//! Run from `agent-os/` against a coordinator with the vault enabled
//! (`COVENANT_COMPUTE_VAULT=1`):
//!
//! `cargo run -p covenant-compute-buyer --example vault_roundtrip`
//!
//! - `COVENANT_COMPUTE_COORDINATOR_URL` — default `http://127.0.0.1:8720`
//!
//! Exits 0 only when the secret stored, fetched back byte-for-byte, and
//! deleted cleanly.

use std::time::Duration;

use covenant_compute_buyer::{vault_delete, vault_fetch, vault_list, vault_store, BuyerConfig};
use covenant_compute_protocol::VaultKey;
use covenant_identity::LocalIdentity;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::var("COVENANT_COMPUTE_COORDINATOR_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8720".into());
    let http = reqwest::Client::new();
    let config = BuyerConfig {
        coordinator_url: base.clone(),
        poll_interval: Duration::from_millis(200),
        referral_code: None,
        rpc_url: None,
    };

    let buyer = LocalIdentity::generate("vault-roundtrip@example");
    let key = VaultKey::random();
    println!("owner : {}", buyer.agent_id().pubkey_base58());
    println!(
        "key   : {} (keep this — the coordinator never receives it)",
        key.to_b58()
    );

    let label = "example-token";
    let secret = b"sk-live-example-do-not-use";

    vault_store(&http, &config, &buyer, &key, label, secret).await?;
    println!("stored '{label}'");

    let listed = vault_list(&http, &config, &buyer).await?;
    let labels: Vec<&str> = listed.iter().map(|m| m.label.as_str()).collect();
    println!("listed {} secret(s): {labels:?}", listed.len());

    let opened = vault_fetch(&http, &config, &buyer, &key, label).await?;
    anyhow::ensure!(
        opened == secret,
        "fetched secret did not match the original"
    );
    println!("fetched and opened '{label}' — matches the original");

    vault_delete(&http, &config, &buyer, label).await?;
    anyhow::ensure!(
        vault_fetch(&http, &config, &buyer, &key, label)
            .await
            .is_err(),
        "the secret should be gone after delete"
    );
    println!("deleted '{label}' — gone");
    println!("\nvault round-trip ok against {base}");
    Ok(())
}
