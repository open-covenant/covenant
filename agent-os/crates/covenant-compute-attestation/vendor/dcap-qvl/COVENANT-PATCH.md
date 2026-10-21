# Covenant patch to dcap-qvl 0.6.1

Source: https://github.com/Phala-Network/dcap-qvl, commit `6ac45907f814e1c3e8bfc1b0e3c6a99710d4ef9f`, published crates.io package `dcap-qvl = 0.6.1`.
Original crate archive SHA-256: `26bb988bf78cd9a44477ee4bbfa9768335c3b58df75860ea048aee3356c90d7c`.
The upstream MIT license is preserved in `LICENSE`. Files are vendored from that published archive.

Covenant's local patch replaces the serde_json `preserve_order` feature with `raw_value`. The optional collateral fetcher now stores the signed `tcbInfo` and `enclaveIdentity` payloads as `Box<RawValue>` and passes their exact bytes, including original whitespace, escapes, and field order, to the existing verifier. It never reconstructs signed JSON through a `Value` serialization. The two response structs live in a private `collateral_json` module; an attestation integration test compiles that exact source to exercise both payloads without enabling the optional HTTP fetcher.

This avoids enabling insertion-ordered serde_json maps across Covenant's Cargo workspace and changing legacy protocol signing/hash bytes for dynamic JSON. No cryptographic verification or TCB policy was relaxed. The parent crate pins this path dependency to exactly version 0.6.1. Rebase this small patch and rerun both raw-payload and existing attestation tests before any vendor upgrade.

Trailing whitespace in upstream documentation, a shell script and web-test scaffolding was normalized for repository whitespace checks; verifier fixtures and cryptographic bytes were not rewritten.
