# Review Artifact Signing

Autonomy review artifacts are generated unsigned by default. Signed review artifacts are supported only when a human-approved project public key is supplied to the verifier.

Generate the unsigned artifact:

```bash
node agent-os/scripts/autonomy-review-artifact.mjs <task-id> --json
```

Verify an unsigned artifact:

```bash
node agent-os/scripts/autonomy-verify-review-artifact.mjs --stdin
```

Verify a signed artifact when a trusted public key has been approved:

```bash
node agent-os/scripts/autonomy-verify-review-artifact.mjs \
  --stdin \
  --trusted-public-key-spki-base64 "$COVENANT_REVIEW_PUBLIC_KEY_SPKI_BASE64"
```

The trusted key value is the base64-encoded DER SPKI public key. The verifier checks the artifact's `signing.public_key_spki_sha256` against that trusted key before verifying the ed25519 signature.

## Signed Envelope

Signed artifacts use the existing `autonomy_review_artifact` payload with:

```json
{
  "signing": {
    "status": "signed",
    "schema": "covenant.autonomy-review-signature.v1",
    "algorithm": "ed25519",
    "key_id": "project-review-key",
    "public_key_spki_sha256": "<sha256 hex of trusted SPKI DER>",
    "signed_at": "2026-01-01T00:00:00.000Z",
    "custody": {
      "policy": "docs/provenance/review-artifact-signing.md",
      "public_key_source": "human-approved-project-key",
      "human_approval_required": true
    },
    "signature": "<base64 ed25519 signature>"
  }
}
```

The signature payload is the full JSON artifact with `signing.signature` set to an empty string. That binds the task snapshot, transition events, source paths, digests, key id, key digest, custody policy, and signed timestamp.

## Custody Boundary

Automation must not create, publish, rotate, or use a project review signing key without explicit human approval. The validator uses an in-memory fixture key only to prove verifier behavior; it does not write key material to disk and does not establish a project trust root.

Before signed review artifacts can become release evidence, humans must approve:

- project review signing key custody;
- public key publication location;
- key id, rotation, and revocation policy;
- who may sign review artifacts;
- whether signed review artifacts are release-grade evidence.
