# SEV-SNP test vectors

Everything here is generated locally by `regenerate_fixtures` in
`tests/snp_vectors.rs`:

```
cargo test --release -p prism-attestation -- --ignored regenerate_fixtures
```

Use `--release`. The run generates four RSA-4096 keys, which an unoptimised
build makes slow enough to look hung. Keys are freshly generated on every run,
so regenerating rewrites all of it.

`test-ark.der` is only ever an anchor for a `Policy::for_tests()`, which no
service constructs. Production anchors at `roots/amd-ark-genoa.der` and nothing
else. `vcek-key.pkcs8.der` and `attacker-vcek-key.pkcs8.der` are chip keys for
these vectors alone; they sign nothing a control plane would accept.

The certificates are built by a small DER writer in the test file rather than by
rcgen, which cannot sign with RSASSA-PSS over SHA-384 and cannot place the AMD
extensions. That is also what lets each negative vector differ from the positive
one in exactly one field:

| fixture | differs by |
| --- | --- |
| `vcek-wrong-hwid.der` | HWID names another chip |
| `vcek-low-svn.der` | microcode SVN one below the floor |
| `ask-pkcs1v15.der` | signed PKCS#1 v1.5 instead of PSS |
| `attacker-*.der` | rooted at an ARK we do not pin |

The ARK in `roots/` is a placeholder generated in this repository with its
private key discarded, by `regenerate_placeholder_ark`, and the launch
measurements in `reference/snp-launch-measurements.json` are placeholders too.
Until both are replaced, a genuine report cannot anchor and cannot match, which
is the intended failure direction. `reference/provenance.json` records that
state, and `the_ceiling_matches_the_evidence_on_file` fails the build if the
trust ceiling ever moves ahead of it.

When a genuine capture is taken from the Dallas platform it lands as
`genuine/report.bin` and `genuine/chain/{vcek,ask,ark}.der`, and
`genuine_genoa_report_verifies` loses its `#[ignore]`.
