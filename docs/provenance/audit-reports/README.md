# Release Audit Reports

This directory is the publication location for per-release audit integrity reports — one file per release tag, named `<tag>.json`. Each file is the JSON output of `covenant audit verify --json`, captured by the release operator against the release daemon and committed before the release tag is pushed.

The report is one of the inputs bound into the signed `covenant.audit-root-attestation.v1` release evidence. Under the release-evidence floor (see [docs/provenance/release-scopes.md](../release-scopes.md)), every release ships three signed artifacts: the release subject manifest, the release-scope manifest, and an audit-root attestation that embeds this report and binds it to the other two.

## Operator ceremony (per release)

1. Land the release-content commit on `main`. Note its full 40-hex SHA — the release tag will point at this commit, and the release-scope manifest must name it in `release.commit`.
2. Generate the release-scope manifest for the tag against that commit and write it to `docs/provenance/release-scopes/<tag>.json`.
3. Run `covenant audit verify --json > docs/provenance/audit-reports/<tag>.json` against the release daemon.
4. Commit both evidence files to `main` and push. This evidence commit comes after the release-content commit and must be pushed before the tag.
5. Create the tag on the release-content commit from step 1 (not on the evidence commit) and push it.
6. Publish the GitHub release. [.github/workflows/sign-release-artifacts.yml](../../../.github/workflows/sign-release-artifacts.yml) then reads both evidence files from `main`, generates the audit-root attestation binding them to the release subject, signs all three blobs with cosign keyless, and uploads payload + `.sig` + `.pem` triples as release assets.

The signing workflow fails loudly — before anything is signed or uploaded — if either evidence file is missing on `main` for the tag, so a forgotten ceremony step cannot produce a half-signed release.

**Regenerate the report for every tag.** The report carries no commit or tag binding of its own; reusing a stale file would sign an audit-root that claims an outdated event count and root hash. The ceremony, not the workflow, is what guarantees freshness.

## Schema

The report must satisfy `parseAuditReport` in [agent-os/scripts/provenance.mjs](../../../agent-os/scripts/provenance.mjs) before an audit-root attestation will embed it:

| Field | Type | Constraint |
|---|---|---|
| `events` | integer | non-negative; must equal `anchors` |
| `anchors` | integer | non-negative; must equal `events` |
| `valid` | boolean | must be `true` |
| `root_hash_hex` | string | 64 lowercase hex characters |
| `failures` | array of strings | must be empty |

A report with `valid: false` or any recorded failure is not release evidence; fix the daemon state first.

## What this does and does not claim

The report exposes structural facts only — event and anchor counts and the audit-log root hash. No audit event contents, principals, or capability payloads appear in the report, in the audit-root attestation, or in any signed release asset.

The cosign signature proves a workflow on this repository bound this report to a specific release subject and release scope at signing time. It does not independently prove the daemon that produced the report was honest — the report content is operator-supplied evidence, and the audit-root attestation records it as such.

## Verification

Each published release carries `audit-root.json`, `audit-root.json.sig`, and `audit-root.json.pem` as assets. Verify with the project identity pins from [docs/provenance/keys/README.md](../keys/README.md):

```bash
cosign verify-blob \
  --certificate audit-root.json.pem \
  --signature   audit-root.json.sig \
  --certificate-identity-regexp '^https://github.com/open-covenant/covenant/' \
  --certificate-oidc-issuer     'https://token.actions.githubusercontent.com' \
  audit-root.json
```

Then confirm the embedded report matches the committed `docs/provenance/audit-reports/<tag>.json` and that `node agent-os/scripts/provenance.mjs audit-root verify --file audit-root.json` passes; it re-validates the embedded release subject, release scope, and report digests.
