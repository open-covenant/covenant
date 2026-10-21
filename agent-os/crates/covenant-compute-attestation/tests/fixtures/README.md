# Attestation test vectors

Everything here is generated locally by `regenerate_fixtures` in
`tests/nvidia_vectors.rs`:

```
cargo test -p prism-attestation -- --ignored regenerate_fixtures
```

Keys are freshly generated on every run, so regenerating rewrites all of it.

`test-root.der` is only ever an anchor for a `Policy::for_tests()`, which no
service constructs. Production anchors at `roots/nvidia-device-identity-root.der`
and nothing else. `leaf-key.pkcs8.der` and `expired-leaf-key.pkcs8.der` are
device keys for these vectors alone; they sign nothing a control plane would
accept.

The vendor root in `roots/` is a placeholder generated in this repository with
its private key discarded, and the reference measurements are placeholders too.
Until both are replaced with values captured from the Dallas H100, a genuine
report cannot anchor and cannot match, which is the intended failure direction.

When that capture happens, it lands as `genuine/report.bin` and
`genuine/chain/{leaf,intermediate,root}.der`, and `genuine_h100_report_verifies`
loses its `#[ignore]`.
