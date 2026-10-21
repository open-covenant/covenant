Covenant Compute 0.5.0 includes the Knowledge compiler executor and the
experimental Phala CPU lease lifecycle. All four packaged commands use the
same version: `covenant-compute`, `covenant-compute-mcp`,
`covenant-compute-node`, and `covenant-compute-stake`.

## Phala CPU leases

The buyer can prepare an exact workload offline, sign its operator and absolute
expiry, submit once through the durable budget, and verify a lease before SSH
access or workspace restoration. The node journals provider operations and
cleanup obligations, collects fresh hardware evidence, and preserves uncertain
outcomes for reconciliation. The guest uses a bounded memory-backed workspace
and refuses access when required storage or swap controls cannot be verified.

This is an experimental implementation, not an announcement of confidential
production capacity. Creation is disabled by default and the served trust
ceiling remains `Open`. KMS policy qualification and an integrated live
lease/settlement/deletion acceptance remain release gates. Installing these
commands does not activate capacity or authorize a purchase.

See the [lease lifecycle guide](https://github.com/open-covenant/covenant/blob/compute-v0.5.0/docs/compute-phala-lease-lifecycle.md)
for preparation, configuration and the remaining qualification requirements.

## Knowledge executor

The node can route the exact Knowledge compiler capability to its isolated,
pinned container executor. Admission checks the expected job shape and the
compiler image before execution; existing unrelated batch routes remain
separate.

## Verify a download

Each platform build checks the extracted executables and bundled audit files
before publication. Linux executables use static musl linking. For Linux
x86-64, verify the matching archive:

```sh
cosign verify-blob \
  --signature covenant-compute-linux-x86_64.tar.gz.sig \
  --certificate covenant-compute-linux-x86_64.tar.gz.pem \
  --certificate-identity https://github.com/open-covenant/covenant/.github/workflows/compute-release.yml@refs/tags/compute-v0.5.0 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  covenant-compute-linux-x86_64.tar.gz
grep '  covenant-compute-linux-x86_64.tar.gz$' checksums.txt | sha256sum -c
gh attestation verify covenant-compute-linux-x86_64.tar.gz --repo open-covenant/covenant
```

Use the matching filename for another platform and `shasum -a 256 -c` on macOS.
Homebrew and the website installer pin releases separately. These notes do not
establish that a tag, signed archive, installer update or deployment has been
published.
