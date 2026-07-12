//! Emit a deterministic CCIP-Read golden vector from the resolver signing core,
//! so the Solidity `OffchainResolver` can be tested against a real signature this
//! crate produced. Fixed key and resolver address, so the output never changes.
//!
//!   cargo run -p covenant-evm-signer --example resolver_vector

use covenant_evm_signer::resolver::{encode_solana_addr_request, ResolverGateway};
use covenant_identity::Secp256k1IssuerKey;

fn hex0x(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn main() {
    let resolver = [0x11u8; 20];
    let node = [0x22u8; 32];
    let solana = [0x33u8; 32];
    let expires: u64 = 1_800_000_000;

    let gw = ResolverGateway::new(
        Secp256k1IssuerKey::from_secret_bytes(&[7u8; 32]).unwrap(),
        resolver,
    );
    let name = b"\x05agent\x0copencovenant\x03eth\x00";
    let request = encode_solana_addr_request(name, &node);
    let resp = gw.resolve_solana(&request, &solana, expires).unwrap();

    println!("resolver = {}", hex0x(&resolver));
    println!("signer   = {}", hex0x(&gw.signer_address()));
    println!("expires  = {expires}");
    println!("request  = {}", hex0x(&request));
    println!("response = {}", hex0x(&resp.abi_encode()));
    println!("result   = {}", hex0x(&resp.result));
}
