# Independent production launch reference

`expected-measurements.json` was derived on 2026-10-20 using the pure
measurement APIs in official dstack source commit
`7f143148cc49f4a09799958835d310e050137cb3`. The input is the public
`vm_config` from the existing Phala diagnostic CVM. No quoted register or
event-log digest is used as an expected measurement.

Before replay, the embedded checksum manifest and CBOR were compared byte for
byte with `sha256sum.txt` and `measurement.tdx.cbor` in the official
[`mkosi-os-v0.6.1` release](https://github.com/Dstack-TEE/dstack/releases/tag/mkosi-os-v0.6.1):

- Archive `dstack-0.6.1.tar.gz` SHA-256:
  `7583f51c717011b8086c2dd4d1e73fbbe7bf2d6d2420b90858d538c26efcca41`.
- SHA-256 of the exact checksum manifest, also the pinned OS image hash:
  `ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6`.

To reproduce, check out that exact upstream source, create a temporary Rust
package outside the repository, copy `main.rs` to its `src/main.rs`, and copy
the retained `Cargo.lock`. Use this manifest, replacing both absolute source
paths with the verified checkout:

```toml
[package]
name = "covenant-independent-launch-replay"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
serde_json = "1"
hex = "0.4"
dstack-mr = { path = "/verified/dstack-source/dstack/dstack-mr" }
dstack-types = { path = "/verified/dstack-source/dstack/dstack-types" }
```

Run `cargo run --locked -- /absolute/path/to/vm-config.json`. Compare its JSON
output with `expected-measurements.json`. The wrapper requires the explicit
lite profile and the pinned production OS hash, verifies the embedded document
against the manifest, checks the OVMF variant, and recomputes ACPI hashes from
the VM configuration before deriving MRTD and RTMR0–2.

These values match the diagnostic node's public Info response. This comparison
alone does not authenticate a live quote. Intel signature/collateral validation,
strict TCB policy, runtime event replay, approved application measurements,
lease/key binding and actual SSH acceptance remain separate requirements.
The compiled Covenant served ceiling remains `Open`.
