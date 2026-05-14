# Release Scope Manifests

This directory is the publication location for `covenant.release-scope.v1` manifests — one file per release tag, named `<tag>.json`. The schema, generation rules, and verifier contract are defined in [docs/provenance/release-scopes.md](../release-scopes.md).

The directory is intentionally empty at v0.1.0. Manifests will land when the generator script and signing workflow extension are wired up. Each published manifest will ship with sibling `<tag>.json.sig` and `<tag>.json.pem` artifacts produced by `cosign sign-blob` via the GitHub Actions release-signing workflow.

Until then, no release-scope claims are made.

## Verification (when manifests exist)

```bash
cosign verify-blob \
  --certificate <tag>.json.pem \
  --signature   <tag>.json.sig \
  --certificate-identity-regexp '^https://github.com/open-covenant/covenant/' \
  --certificate-oidc-issuer     'https://token.actions.githubusercontent.com' \
  <tag>.json
```

A passing verify proves the manifest was signed by a GitHub Actions workflow on this repository. Verifiers cannot independently recompute `task_set_sha256` without access to the project's task corpus, which is intentional (the corpus is internal-loop only).
