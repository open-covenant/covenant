# Release Evidence Bundles

Release evidence bundles record the facts used to accept, reject, or supersede an alpha candidate.

Create one directory per release candidate:

```bash
node agent-os/scripts/alpha-release-bundle.mjs v0.1.0-alpha.1
```

Expected contents:

- `evidence.json`: output from `node agent-os/scripts/alpha-release-evidence.mjs --json`;
- validation notes with command outcomes and skipped live prerequisites;
- links to provenance envelopes or audit-root attestations generated for the candidate;
- the release decision.

Do not store private keys, tokens, local host paths, local usernames, or unpublished credential names in release evidence.
