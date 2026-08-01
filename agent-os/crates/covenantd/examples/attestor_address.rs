//! Print the SpendGrant attestor's EVM address — the value to set as a deployed
//! `SpendGrantEscrow`'s `attestor` role. Loads (or on first run mints) the
//! daemon's secp256k1 attestor key at the given path, then prints its address.
//!
//!   cargo run -p covenantd --example attestor_address -- ~/.config/covenant/spendgrant-attestor.key

use std::path::Path;

use covenantd::spend_grant::SpendGrantAttestor;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: attestor_address <key-path>");
    let attestor =
        SpendGrantAttestor::load_or_create(Path::new(&path)).expect("load or create attestor key");
    let addr = attestor.address();
    let hex: String = addr.iter().map(|b| format!("{b:02x}")).collect();
    println!("0x{hex}");
}
