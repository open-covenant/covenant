use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=COVENANT_KEY_BROKER_POLICY");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let policy = match env::var_os("COVENANT_KEY_BROKER_POLICY") {
        Some(path) => {
            let path = PathBuf::from(path).canonicalize().expect("policy path");
            println!("cargo:rerun-if-changed={}", path.display());
            let bytes = fs::read(path).expect("read embedded broker policy");
            assert!(bytes.len() <= 1024 * 1024, "oversized broker policy");
            bytes
        }
        None => Vec::new(),
    };
    // A broker with no embedded policy can inspect files but cannot serve.
    // Loading policy supplied by the parent at runtime would bypass PCR pins.
    fs::write(out.join("broker-policy.json"), policy).expect("embed broker policy");
}
