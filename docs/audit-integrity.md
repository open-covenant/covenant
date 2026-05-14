# Audit Integrity

Covenant records daemon audit events as JSONL. Each event stays in the primary audit file, and the JSONL file remains tail-friendly for local operators and tools.

The audit crate now writes a local hash-chain sidecar next to the event log. For the default `events.jsonl` path, the sidecar is `events.chain.jsonl`.

## Chain Format

Each sidecar row is an `AuditChainEntry`:

| Field | Meaning |
|---|---|
| `index` | Zero-based event position in the retained audit log. |
| `event_id` | Event UUID from the audit row. |
| `timestamp_ms` | Event timestamp from the audit row. |
| `event_hash_hex` | SHA-256 hash of the exact retained JSONL event line. |
| `previous_hash_hex` | Previous chain root, or 64 zeroes for the first entry. |
| `chain_hash_hex` | SHA-256 hash of `previous_hash_hex + "\n" + event_hash_hex`. |

The sidecar is append-only during normal writes. If the sidecar is missing or has a different length from the event log when a new event is recorded, Covenant rebuilds it over the retained events before appending the new anchor. Retention purge rewrites both the retained audit file and its sidecar, so a valid retained log remains valid after old rows are intentionally removed.

Retention automation should use the machine-readable purge form:

```bash
covenant audit purge --before-ms 1700000000000 --json
```

```json
{
  "kind": "audit_purged",
  "before_ms": 1700000000000,
  "purged": 0
}
```

## Verification

Operators can verify the local chain through all daemon surfaces:

```bash
covenant audit verify
```

The CLI prints a bare `AuditIntegrityReport` JSON object by default. Use `--json` for a stable envelope:

```bash
covenant audit verify --json
```

```json
{
  "kind": "audit_integrity",
  "report": {
    "events": 0,
    "anchors": 0,
    "valid": true,
    "root_hash_hex": "<hex>",
    "failures": []
  }
}
```

```http
GET /audit/verify
Authorization: Bearer <operator-token>
```

The IPC request is `VerifyAuditIntegrity`. All surfaces return an `AuditIntegrityReport`:

| Field | Meaning |
|---|---|
| `events` | Number of retained event rows read from the audit log. |
| `anchors` | Number of sidecar rows read from the chain file. |
| `valid` | `true` when the recomputed chain matches every sidecar row. |
| `root_hash_hex` | Current root hash over the retained audit log. |
| `failures` | Deterministic mismatch, missing-entry, and dangling-anchor diagnostics. |

The daemon restricts integrity verification to the operator identity because the report exposes global audit metadata. A non-operator peer receives an error.

## Security Boundary

This is local tamper evidence, not public non-repudiation.

The chain detects edits to retained audit rows, missing sidecar entries, and sidecar mismatch after events have been anchored locally. It does not stop a host-level attacker from deleting both the audit log and the sidecar, replacing both files together, or rolling the machine back to an older filesystem snapshot.

The current implementation can generate and verify `audit-root-attestation.v1` payloads from `covenant audit verify` output as unsigned envelopes. Public release-grade signing has not shipped yet; the path is sigstore keyless via cosign, defined alongside the other project-identity signing in [docs/provenance/keys/](./provenance/keys/). Until the signing workflow lands, audit-root attestations stay unsigned and are treated as local integrity evidence only.

## Public Root Signing Direction

Release-candidate `audit-root-attestation.v1` payloads will be signed by a GitHub Actions workflow using cosign with sigstore keyless (Fulcio short-lived certs + Rekor transparency log), the same identity model the release tarballs already use. The OIDC issuer pin is `https://token.actions.githubusercontent.com` and the certificate identity must match `^https://github.com/open-covenant/covenant/`.

The generator and unsigned-verifier exist today in `agent-os/scripts/provenance.mjs`:

```bash
covenant audit verify > audit-report.json
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --task <task-id> \
  --commit <commit> \
  --out docs/provenance/audit-roots/<commit>-audit-root.json

node agent-os/scripts/provenance.mjs audit-root verify \
  --file docs/provenance/audit-roots/<commit>-audit-root.json
```

Once the signing workflow ships, a signed audit-root attestation will be a triple — `<file>.json`, `<file>.json.sig`, `<file>.json.pem` — and a verifier will run:

```bash
cosign verify-blob \
  --certificate <file>.json.pem \
  --signature   <file>.json.sig \
  --certificate-identity-regexp '^https://github.com/open-covenant/covenant/' \
  --certificate-oidc-issuer     'https://token.actions.githubusercontent.com' \
  <file>.json
```

Public non-repudiation depends on the Rekor log entry; verifiers can re-check the entry exists for additional confidence.

Release-target audit roots can also bind an embedded `covenant.provenance.release.v1` release subject digest. The verifier checks repository, release id, commit, artifact metadata, validation evidence, and the embedded `releaseSubjectSha256`; the human custody steps for accepting a release-target root are kept in the project's release operator handbook.
