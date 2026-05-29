# Releases

Covenant releases are versioned snapshots of the daemon, libraries, operator surfaces, on-chain artifacts, and the metadata required to reproduce them.

## Versioning

- Public crates follow [SemVer](https://semver.org/).
- On-chain program deployments are tracked by network, commit SHA, and IDL digest.
- Breaking changes to public wire formats, capability shapes, or on-chain interfaces require migration notes and maintainer approval.

## Release checklist

- `cargo build  --workspace --exclude covenant-settlement-program`
- `cargo test   --workspace --exclude covenant-settlement-program`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
- `node agent-os/scripts/provenance.mjs verify-all`
- `pnpm --dir landing build`
- `anchor build` for on-chain program changes
- Live integration coverage exercised against real backends, where applicable
- Security review notes for changes to identity, capability, audit, or settlement surfaces

## Publishing

Releases include a concise summary, upgrade notes, test evidence, provenance envelopes for release-producing commits, and links to reproducible artifacts. Do not publish generated artifacts that are not reproducible from the tagged source tree.

Tagged releases are signed with [cosign](https://github.com/sigstore/cosign) keyless OIDC (Sigstore) — the signing identity is the GitHub Actions workflow that built the release. Verify any release artifact with:

```bash
cosign verify-blob \
  --certificate <artifact>.pem \
  --signature   <artifact>.sig \
  --certificate-identity-regexp "^https://github.com/open-covenant/covenant/" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  <artifact>
```

Provenance envelopes recorded under `docs/provenance/` are consistency evidence (commit + build inputs + audit chain head), complementary to the cosign signature.
