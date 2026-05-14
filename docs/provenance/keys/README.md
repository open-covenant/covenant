# Project Signing

The Covenant project signs project-identity artifacts using **sigstore keyless** via cosign. There is no long-lived project signing key. Each signature is produced by a GitHub Actions workflow on this repository using OpenID Connect; cosign exchanges the OIDC token for a short-lived certificate issued by the public-good Fulcio CA, signs the artifact, and pushes the signature record to the Rekor public transparency log.

This contract is defined in [docs/internal/decisions/0006-project-signing-key-custody.md](../../internal/decisions/0006-project-signing-key-custody.md) (engineering-internal). The verification command line below is the public part.

## Identity Pins

Verifiers must accept signatures only when both pins match:

| Pin | Required value |
|---|---|
| OIDC issuer | `https://token.actions.githubusercontent.com` |
| Certificate identity (subject regex) | `^https://github.com/open-covenant/covenant/` |

The first pin restricts to GitHub Actions OIDC. The second pin restricts to workflows running on this project's repository. A signature that fails either pin is not a project signature.

## What Gets Signed

| Artifact kind | Signed by | Workflow |
|---|---|---|
| Release tarballs and combined checksum file | Every release tag push | [.github/workflows/release.yml](../../../.github/workflows/release.yml) |
| `covenant.provenance.release.v1` release subject manifests | Release tag push (follow-up workflow) | _planned_ |
| `covenant.release-scope.v1` release-scope manifests | Release tag push (follow-up workflow) | _planned_ |
| `covenant.audit-root-attestation.v1` audit-root attestations | Release tag push (follow-up workflow) | _planned_ |
| `covenant.autonomy-review-signature.v1` review artifacts for release-scope tasks | Release tag push (follow-up workflow) | _planned_ |

Routine autonomy review artifacts produced outside a named release scope stay unsigned. Their durable evidence is the `covenant.autonomy-review-artifact.v1` envelope plus the transition event chain — no signature.

## Verification

Each signed artifact ships as a triple: the payload, a detached signature, and the Fulcio certificate that signed it.

```bash
cosign verify-blob \
  --certificate <artifact>.pem \
  --signature   <artifact>.sig \
  --certificate-identity-regexp '^https://github.com/open-covenant/covenant/' \
  --certificate-oidc-issuer     'https://token.actions.githubusercontent.com' \
  <artifact>
```

This validates the signature against the public key embedded in the certificate, confirms the certificate was issued by Fulcio during a validity window that includes the Rekor log entry's `integratedTime`, and binds the signature to the OIDC identity that produced it.

For release tarballs the verification commands are also printed in each GitHub release body.

## Why Sigstore Keyless

- No private key for the project to custody. Operator compromise of a workstation does not compromise the signing identity.
- Identities are short-lived (Fulcio certificates expire within minutes of issue), so there is no long-lived-key compromise window.
- The Rekor transparency log provides public, append-only evidence that a signature was created during a specific time window. Verifiers can re-check the log entry exists.
- Authorization to sign is "who can push tags / dispatch workflows", enforced by GitHub branch protection. Rotation is changing the branch protection rule.

## Legacy Infrastructure

This directory previously contained `keys.json` and `keys.template.json` describing a long-lived ed25519 project key under operator custody. That model required dedicated air-gapped hardware the project does not have and is not in use. The files have been removed. The internal validator `validate-project-keys.mjs` remains in the repository as dormant infrastructure so a future decision could re-enable long-lived keys without re-shipping the manifest schema.

Any signed artifact produced before sigstore keyless was adopted (none exist) would remain verifiable under its original contract.
