# Project Signing Keys

This directory is the canonical publication location for project-controlled signing keys. Custody, rotation, and authorized-signer policy are tracked internally.

The repository is the source of truth: any verifier with the repository can read the trusted public key from here and verify signatures against the same envelope schemas the project produces.

## Layout

- `keys.json` — manifest of every key the project has published. Schema `covenant.project-keys.v1`. Tracked in git.
- `<key_id>.spki.base64` — DER SubjectPublicKeyInfo of one published key, base64-encoded, one per file. Tracked in git.

`keys.json` is the index. Each entry binds a `key_id` to the SPKI file, fingerprint, validity window, status, and authorized signer.

## Manifest Schema

```json
{
  "schema": "covenant.project-keys.v1",
  "keys": [
    {
      "key_id": "project-review-key-2026-01",
      "algorithm": "ed25519",
      "spki_file": "project-review-key-2026-01.spki.base64",
      "spki_sha256": "<hex sha256 of the DER SPKI bytes>",
      "not_before": "2026-01-01T00:00:00.000Z",
      "not_after": "2027-01-01T00:00:00.000Z",
      "status": "active",
      "authorized_signer": "open-covenant-release-operator",
      "purposes": ["autonomy-review-signature", "audit-root-attestation", "release-subject"]
    }
  ]
}
```

Fields:

- `key_id`: stable identifier for the key. Never re-use after rotation or revocation.
- `algorithm`: must be `ed25519` for now.
- `spki_file`: filename of the DER SPKI base64 file in this directory.
- `spki_sha256`: hex SHA-256 of the DER SPKI bytes, used to bind signatures to keys.
- `not_before`: ISO timestamp when the key becomes valid for signing.
- `not_after`: ISO timestamp after which the key must not produce new signatures.
- `status`: one of `active`, `retired`, `revoked`.
- `revoked_at`: ISO timestamp when the key was revoked. Required when `status` is `revoked`. Must be absent otherwise.
- `revoked_reason`: short rationale string. Required when `status` is `revoked`. Must be absent otherwise.
- `authorized_signer`: role string. Currently always `open-covenant-release-operator`.
- `purposes`: list of envelope kinds the key may sign. Subset of `autonomy-review-signature`, `audit-root-attestation`, `release-subject`.

## Computing `spki_sha256`

`spki_sha256` is the lowercase hex SHA-256 of the **decoded DER bytes** of the SPKI file, not of the base64 text. To reproduce it from a published file:

```bash
openssl enc -d -base64 -A -in <key_id>.spki.base64 | openssl dgst -sha256
```

On systems without OpenSSL, the same result comes from a portable decode-then-hash pipeline:

```bash
base64 -d <key_id>.spki.base64 | sha256sum   # GNU coreutils
base64 -d <key_id>.spki.base64 | shasum -a 256   # BSD / macOS
```

The 64-character lowercase hex output is the value recorded in `spki_sha256` for that entry. An empty `keys` array is the valid pre-publication state, in which case no fingerprint exists yet.

## Operational Steps

The release-operator role generates and publishes keys. Automation does not generate, sign with, or rotate the project key. The operator:

1. Generates ed25519 key material on a trusted offline machine.
2. Computes the DER SPKI bytes and base64-encodes them into `<key_id>.spki.base64`.
3. Computes the SHA-256 of the DER SPKI bytes.
4. Adds an `active` entry to `keys.json` with the parameters above.
5. Commits the SPKI file and the manifest update with a single tracked commit.
6. Announces rotation 30 days before the prior key's `not_after`. The new key is published as a second `active` entry; the prior key's status is changed to `retired` after the prior key's `not_after`.

Compromise response:

1. Mark the affected entry `revoked` with `revoked_at` and `revoked_reason`.
2. Publish the change in a tracked commit on the same day as the determination.
3. Generate and publish a replacement key with a new `key_id`.

## Validation

The `keys.json` manifest is validated by internal tooling that confirms schema correctness, that each `active` entry has a matching SPKI file whose SHA-256 equals `spki_sha256`, that retired/revoked entries have well-formed status fields, that no `key_id` is re-used, and that no entry mixes status fields it should not have. The validator does not generate keys, fetch external resources, or modify the manifest.
