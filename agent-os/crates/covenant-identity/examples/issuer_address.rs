//! Print the EVM address of the secp256k1 issuer key at
//! `$COVENANT_EVM_ISSUER_KEY`, creating it (0600) on first run. The address is
//! the `TRUSTED_ATTESTOR` a Base verifier checks against; the key signs bond
//! receipts and reputation projections and never sends a transaction.

use std::path::PathBuf;

use covenant_identity::Secp256k1IssuerKey;

fn main() {
    let path = PathBuf::from(
        std::env::var("COVENANT_EVM_ISSUER_KEY").expect("set COVENANT_EVM_ISSUER_KEY"),
    );
    let key = Secp256k1IssuerKey::load_or_create(&path).expect("load or create issuer key");
    print!("0x");
    for b in key.address() {
        print!("{b:02x}");
    }
    println!();
}
