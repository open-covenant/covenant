Covenant Compute 0.3.1 adds durable, caller-selected GPU rental IDs for automated
checks. Existing inference, workspace and ordinary rental commands are unchanged.

## Automated rentals

`covenant-compute lease open --job-id UUID` requires a configured durable daily
budget. The buyer reserves the UUID before making a single submission, with
HTTP redirects and transport retries disabled. A later invocation with the same
UUID is refused, even after restarting the client. If the response is lost,
reconcile the original rental with `lease view`, `lease close` and `payout
verify`; do not create another UUID to bypass an uncertain outcome.

The UUID must be canonical, lowercase and nonnil. Existing requests that omit
`--job-id` retain their previous behavior and signing format.

The broker can emit provider-readable SSH host-key evidence for one explicitly
configured buyer. Its challenge binds that buyer and job UUID; the feature is
off by default. The recurring canary independently checks the provider's job
label, instance identity and authenticated startup log before accepting the key.
This establishes the SSH endpoint binding, not hardware attestation.

## Included commands and verification

Each archive contains `covenant-compute`, `covenant-compute-mcp`,
`covenant-compute-node` and `covenant-compute-stake`, all reporting `0.3.1`, plus
the license, notice, command guide and these notes. Archives cover Apple silicon
and Intel macOS, and x86-64 and ARM64 Linux. Linux executables are static musl.

Each archive and `checksums.txt` has a cosign signature and certificate. Verify
the matching archive against the release workflow identity:

```sh
cosign verify-blob \
  --signature covenant-compute-linux-x86_64.tar.gz.sig \
  --certificate covenant-compute-linux-x86_64.tar.gz.pem \
  --certificate-identity https://github.com/open-covenant/covenant/.github/workflows/compute-release.yml@refs/tags/compute-v0.3.1 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  covenant-compute-linux-x86_64.tar.gz
grep '  covenant-compute-linux-x86_64.tar.gz$' checksums.txt | sha256sum -c
gh attestation verify covenant-compute-linux-x86_64.tar.gz --repo open-covenant/covenant
```

Installing this release does not schedule rentals or activate spending. The
[canary operations guide](https://github.com/open-covenant/covenant/blob/compute-v0.3.1/deploy/compute-canary/README.md)
describes the separately configured pilot, reservations, cleanup and alerts.
Production acceptance and the public release are recorded separately.
