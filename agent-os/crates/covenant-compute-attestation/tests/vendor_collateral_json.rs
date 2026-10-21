//! Compile the patched vendor payload parser directly, without enabling the unused HTTP client.
extern crate alloc;
#[path = "../vendor/dcap-qvl/src/collateral_json.rs"]
#[allow(dead_code)]
mod collateral_json;
