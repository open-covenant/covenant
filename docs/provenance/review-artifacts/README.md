# Release-Scope Review Artifacts

This directory is the publication staging location for `covenant.autonomy-review-signature.v1` review-artifact payloads that belong to a named release scope — one subdirectory per release tag, one payload per task:

```
docs/provenance/review-artifacts/<tag>/<task-id>.review.v1.json
```

The payload naming and signed-envelope contract are defined in the engineering-internal review-artifact signing contract; the public verification command line is below. Routine autonomy review artifacts produced outside a named release scope stay unsigned and are never committed here.

## How payloads land here

1. The engineering loop generates unsigned review artifacts for the tasks bound by a release scope (`node agent-os/scripts/autonomy-review-artifact.mjs <task-id> --json`).
2. The payloads for release-scope-tagged tasks are committed to `docs/provenance/review-artifacts/<tag>/` on `main` before the release tag is pushed, alongside the release-scope manifest (`docs/provenance/release-scopes/<tag>.json`) and audit report (`docs/provenance/audit-reports/<tag>.json`).
3. On release publication, [.github/workflows/sign-release-artifacts.yml](../../../.github/workflows/sign-release-artifacts.yml) stages every `<task-id>.review.v1.json` payload for the tag, signs each with `cosign sign-blob` (sigstore keyless), and uploads the triple — payload, `.sig`, `.pem` — to the GitHub release.

A release whose scope tags no review artifacts is still valid: the workflow records a zero count and ships without signed review artifacts. Only files matching `*.review.v1.json` and carrying the `covenant.autonomy-review-signature.v1` schema marker may appear in a tag directory; anything else fails the signing workflow.

## Verification

```bash
cosign verify-blob \
  --certificate <task-id>.review.v1.json.pem \
  --signature   <task-id>.review.v1.json.sig \
  --certificate-identity-regexp '^https://github.com/open-covenant/covenant/' \
  --certificate-oidc-issuer     'https://token.actions.githubusercontent.com' \
  <task-id>.review.v1.json
```

A passing verify proves the payload was signed by a GitHub Actions workflow on this repository. The identity pins are documented in [docs/provenance/keys/README.md](../keys/README.md).

The directory is intentionally empty until the first release scope tags review artifacts. Until then, no signed-review-artifact claims are made.
