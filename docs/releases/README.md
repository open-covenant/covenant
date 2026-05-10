# Release Evidence Bundles

Release evidence bundles record the facts used to accept, reject, or supersede an alpha candidate.

Create one directory per release candidate:

```bash
node agent-os/scripts/alpha-release-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/alpha-release-validate-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/validate-alpha-release-evidence.mjs
```

Expected contents:

- `evidence.json`: versioned `covenant.alpha-release-evidence.v1` output from `node agent-os/scripts/alpha-release-evidence.mjs --json`;
- `manifest.json`: versioned `covenant.alpha-release-manifest.v1` digest manifest for bundle files;
- validation notes with metadata matching `evidence.json`, command outcomes, alpha readiness blockers, and skipped live prerequisites;
- links to provenance envelopes or audit-root attestations generated for the candidate;
- the release decision.

The manifest records relative bundle file paths, byte counts, and SHA-256 digests for every regular file in the bundle except `manifest.json` itself. It is local integrity evidence, not a signature or transparency-log entry. The bundle validator recomputes the digests and rejects stale manifests or unmanifested bundle files before accepting release evidence.

Readiness blocker ids must stay under `## Alpha Readiness`. Accepted bundles require `evidence.json` to report `readiness.ready: true`. Use
`--allow-blocked-readiness` only for draft blocker review:

```bash
node agent-os/scripts/alpha-release-validate-bundle.mjs v0.1.0-alpha.1 --allow-draft --allow-pending --allow-blocked-readiness
```

The `Status`, `Generated`, `Candidate commit`, `Branch`, `Dirty files`, and `Alpha readiness` header lines in `validation.md` must match `evidence.json`.

Gate lines in `validation.md` must stay under `## Required Gates` and keep the scaffold format:

```text
- [x] `<command>` - result: passed
- [x] `<command>` - result: failed
- [x] `<command>` - result: skipped: <reason>
```

`result: pending` is valid only with `--allow-pending`.
Bundles with decision `accepted` must mark every required gate as checked and `result: passed`. Use `rejected` or `superseded` for evidence that records failed or skipped gates.

`node agent-os/scripts/validate-alpha-release-evidence.mjs` checks both sides of the contract: draft blocker-review evidence must validate only with explicit overrides, and a synthetic clean/ready accepted bundle must validate with no overrides.

Do not store private keys, tokens, local host paths, local usernames, or unpublished credential names in release evidence.
