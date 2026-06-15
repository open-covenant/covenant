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

## Privileged Action Coverage

The hash chain proves that the recorded audit log has not been tampered with. A separate question is *coverage*: does every privileged daemon action actually record something on that log? Covenant pins this with a drift-guarded inventory test (`privileged_action_audit_inventory_pins_exposure_and_tracks_unaudited_gaps` in `covenantd`) that classifies every IPC request by its success-path audit exposure. The classifier is an exhaustive match over the request type, so a new request variant fails to compile until it is classified — coverage cannot silently shrink as handlers are added.

Each privileged action falls into one of three tiers:

| Tier | Meaning |
|---|---|
| Action-audited | Emits an action-specific row on success — for example `IntentDispatched`, `CapabilityGranted`, `PeerRevoked`, `OperatorTokenRotated`, `MemoryRepairApplied`, `MemoryCompactionApplied`, `ExternalPaymentSettled`, the settlement and memory backfill rows, and the A2A repair rows. |
| Authorization-audited | The success path is recorded only by the `CapabilityCheck` row (`passed = true`) emitted when the action's capability is verified. The row also answers *who* authorized the action and *under which rule*: `authorized_by` lists, for each granted action, the identity that signed the matching grant (`granted_by_display`) and the base58 signature identifying the exact signed capability (`signature_b58`). `authorized_by` is empty on a failed check and always present on the wire, so a `passed = true` row can never silently omit its approver and rule. The authorized attempt — and its approver and rule — is on the chain, but there is no action-outcome row. Covers `CallTool`, the operator purges (`PurgeMemory`, `PurgeAudit`, `PurgeCapabilities`, `PurgePeers`), `FlushReceipts`, `SignAttestation`, `SendA2ATask`, `PostA2AResult`, and `CompactA2A`. |
| Unaudited | Records nothing on the success path. Tracked below. |

Read-only queries are not privileged actions and are excluded. Some are capability-gated and therefore also emit a `CapabilityCheck` row, but that is authorization logging rather than an action record.

### Tracked coverage gaps

The following privileged actions currently record nothing on their success path. They are enumerated explicitly in the inventory test so a new gap cannot land silently:

- **`Authenticate` (success).** Authentication *failures* are audited (`AuthenticationFailed`); a successful handshake is not. Every action a peer subsequently takes is individually audited, so a successful auth carries no standalone accountability requirement today.
- **`RevokeCapability` (success).** A rejected revoke is audited (`CapabilityRevokeRejected`); a successful revoke is not. Revocation is a privilege change and should leave a record — closing this gap means adding a success-path audit row.
- **SAP bridge publishes** (`SapPublishAgent`, `SapPublishAuditRoot`, `SapPublishAttestation`). These cross into the external Synapse Agent Protocol ledger and do not yet emit a local audit row for the publish. The publish authorization model is still being defined; audit emission should land with that work.

Closing a gap means adding the audit emission and removing the entry from both the inventory test and this list.

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

At release time the same generator binds a release tag, the release-subject manifest defined in [provenance/release-subjects.md](./provenance/release-subjects.md), and the release-scope manifest defined in [provenance/release-scopes.md](./provenance/release-scopes.md):

```bash
node agent-os/scripts/provenance.mjs audit-root write \
  --report audit-report.json \
  --release <tag> \
  --release-subject release-subject.json \
  --release-scope release-scopes/<tag>.json \
  --commit <commit> \
  --out docs/provenance/audit-roots/<commit>-audit-root.json
```

`audit-root verify` re-validates each embedded manifest, so a single signature over the audit-root payload covers the audit log, the release artifact set (`releaseSubjectSha256`), and the in-scope autonomy task set (`releaseScopeSha256`) together.

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
