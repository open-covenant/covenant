# Release Artifact Provenance Subjects

Covenant provenance envelopes currently bind autonomy task evidence to a Git commit (`covenant.provenance.v1`). Releases need an additional subject shape so a future signed release can bind:

- the release identifier (tag or release id);
- the canonical source commit;
- the published artifacts and their digests;
- the validation evidence captured at release time.

This document defines the **subject schema** for that future release envelope without choosing key custody, publishing, or signing policy.

## Goals

- Make release artifacts addressable by stable digests (at minimum SHA-256).
- Bind every artifact to one canonical source commit and repository.
- Keep room for multiple artifact formats (tarballs, zips, SBOMs, checksums, signatures) without overclaiming trust.
- Keep key custody and publication as explicitly human-governed work.

## Definitions

- **Release id**: a repository-scoped identifier such as a Git tag (`v0.1.0-alpha.1`) or a build id chosen by the release operator.
- **Artifact**: a release output such as a source tarball, prebuilt binary, container digest, SBOM, or checksums file.
- **Digest**: a cryptographic hash over the artifact bytes (SHA-256 for v1).

## Proposed subject kinds

### `git_tag`

A lightweight subject for tying a tag name to a canonical commit:

- `repository`: `owner/name`
- `tag`: tag name
- `commit`: canonical commit hash

This does not describe artifacts; it is a building block for the release bundle.

### `release_bundle` (planned)

A release bundle subject binds a release id to a commit and a set of artifact digests.

Required fields:

- `repository`: `owner/name`
- `releaseId`: string
- `tag`: optional tag name (when the release is tag-based)
- `commit`: canonical commit hash
- `artifacts`: array of artifact descriptors

Each artifact descriptor must include:

- `name`: stable logical name (e.g. `covenant-linux-amd64`)
- `sha256`: lowercase hex digest
- `sizeBytes`: integer byte count

Optional artifact fields:

- `filename`: published filename
- `mediaType`: MIME type
- `platform`: `{ os, arch }` for binaries
- `uri`: published URL (informational; not trusted for verification)

Verify a release bundle subject against local artifact bytes before using it as release evidence:

```bash
node agent-os/scripts/release-artifact-subject.mjs \
  --subject path/to/release-subject.json \
  --artifact-root path/to/artifacts \
  --json
```

Validate the verifier with fixtures:

```bash
node agent-os/scripts/validate-release-artifact-subject.mjs
```

The verifier checks schema, repository, release id, commit, artifact names, relative filenames, SHA-256 digests, byte counts, and private-string hygiene. It does not sign, upload, publish, or write transparency-log entries.

## Example (illustrative)

```json
{
  "schema": "covenant.provenance.release.v1",
  "generatedAt": "2026-05-09T00:00:00.000Z",
  "subject": {
    "kind": "release_bundle",
    "repository": "open-covenant/covenant",
    "releaseId": "v0.1.0-alpha.1",
    "tag": "v0.1.0-alpha.1",
    "commit": "<40-hex>",
    "artifacts": [
      {
        "name": "covenant-source",
        "filename": "covenant-v0.1.0-alpha.1.tar.gz",
        "sha256": "<64-hex>",
        "sizeBytes": 123456
      },
      {
        "name": "checksums",
        "filename": "checksums.txt",
        "sha256": "<64-hex>",
        "sizeBytes": 987
      }
    ]
  }
}
```

## Non-claims

- This schema does not claim that a release was published.
- This schema does not claim that artifacts were produced by a trusted signer.
- This schema does not define key custody or signature publication.

Those remain release process work that must be explicitly documented and approved.
