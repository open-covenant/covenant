# Identity Provenance

Covenant has local ed25519 identity and peer-token registry primitives. Public identity attestation needs a stricter custody and publication process before it can be trusted outside the local operator boundary.

The current implementation provides a read-only dry-run report:

```bash
node agent-os/scripts/identity-provenance.mjs --json
```

The report emits `covenant.identity-provenance.plan.v1` and records:

- whether the local identity key exists, its byte length, and its filesystem mode;
- peer registry counts for registered, live, and revoked rows;
- peer subjects by public key;
- redacted token prefixes only;
- per-subject rotation history inferred from registered and revoked peer-token rows;
- blockers that must be resolved before public attestation publication.

It intentionally does not export identity seed bytes, full peer tokens, local filesystem paths, or peer display strings. Display values are hashed so operators can correlate repeated rows without leaking local personal or host identifiers.

## Publication Boundary

`identity-provenance.mjs` is not a public attestation publisher. It is local evidence for release planning and security review.

Before public identity attestations can be shipped, a human release operator must approve:

- the project-controlled signing identity or OIDC/Sigstore subject;
- the public-key publication location;
- rotation-retention policy for peer credentials;
- revocation disclosure policy;
- the exact release evidence bundle that will include identity provenance.

Until that process exists, public docs should describe identity provenance as local dry-run evidence, not as a public trust root.
