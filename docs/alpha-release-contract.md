# Alpha Release Contract

Covenant alpha releases are source-built local infrastructure releases. They are intended to give systems engineers and agent-infrastructure contributors a reproducible local control plane to inspect, build, run, and extend. They are not binary distribution releases, SDK stability commitments, production security certifications, or public signing events.

The alpha boundary exists to keep public release language aligned with implementation evidence.

## Release Boundary

An alpha candidate may claim:

- the repository builds from source on supported developer hosts;
- the daemon and CLI can be installed from source into a local prefix with an inspectable install manifest;
- the local daemon, CLI, IPC, HTTP gateway, memory, audit, identity, permissions, peer auth, A2A, budget, and local receipt surfaces are covered by the documented validation profile;
- live tests exist for selected real process, CLI, daemon, HTTP, model, A2A, MCP, and sandbox boundaries where prerequisites are available;
- provenance envelopes and audit-root attestations provide local consistency evidence;
- distributed settlement, package-manager distribution, SDK publication, signed release artifacts, and transparency-log publication remain planned work.

An alpha candidate must not claim:

- production readiness;
- host compromise resistance;
- public non-repudiation without a selected project signing identity;
- immutable audit retention;
- stable external SDK or package APIs;
- public package availability unless that package has actually been published;
- automatic upgrade safety.

## Evidence Bundle

Each alpha candidate should have a release evidence bundle under:

```text
docs/releases/<release-id>/
```

The bundle should contain:

- `evidence.json`: output from `node agent-os/scripts/alpha-release-evidence.mjs --json` using schema `covenant.alpha-release-evidence.v1`;
- `manifest.json`: output from the bundle scaffold using schema `covenant.alpha-release-manifest.v1`, with relative file paths, byte counts, and SHA-256 digests for every regular bundle file except `manifest.json`;
- validation notes listing each required command, outcome, host assumptions, and skipped live prerequisites;
- links to any provenance envelopes or audit-root attestations generated for the candidate;
- the release decision: draft, accepted, rejected, or superseded.

The evidence helper is read-only. It records the schema version, commit, branch, dirty-file count, sanitized alpha readiness state, recommended commands, and release notes. It does not execute the validation commands and does not create a tag or artifact.

Create the bundle scaffold with:

```bash
node agent-os/scripts/alpha-release-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/alpha-release-validate-bundle.mjs v0.1.0-alpha.1
```

The scaffold writes `evidence.json`, `validation.md`, and `manifest.json`. It refuses to overwrite an existing non-empty bundle unless `--force` is supplied.
The validator fails accepted release evidence when the bundle is missing files, contains malformed evidence or manifest data, records stale file digests, omits a regular bundle file from the manifest, records dirty files, has header metadata that diverges from `evidence.json`, leaves gates pending, omits gate outcomes, records skipped gates without reasons, keeps the decision as `draft`, omits readiness blocker ids from `## Alpha Readiness`, records unresolved alpha readiness blockers, or marks the candidate `accepted` while any required gate is failed, skipped, pending, or unchecked. Draft preparation can pass `--allow-blocked-readiness` when the bundle is being used to review blockers rather than accept a release.
The manifest is local digest binding only. It is not a signature, public non-repudiation, or transparency-log publication.
Each required gate line must appear under `## Required Gates` and use `result: passed`, `result: failed`, `result: skipped: <reason>`, or `result: pending`; `pending` is accepted only in draft validation.
The release evidence validator includes a synthetic clean accepted bundle fixture so the acceptance path is tested without depending on the current checkout being clean.

## Minimum Local Gate

Run from the repository root:

```bash
node agent-os/scripts/alpha-release-readiness.mjs
node agent-os/scripts/alpha-release-evidence.mjs --json
node agent-os/scripts/validate-alpha-release-evidence.mjs
bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/validate-autonomy-handoff.mjs
node agent-os/scripts/validate-autonomy-review-artifacts.mjs
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/validate-readme-copy.mjs
node agent-os/scripts/validate-source-installer.mjs
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check
```

A release candidate should have a clean working tree before evidence is accepted. Dirty output from the evidence helper is useful while preparing a release, but it is not release evidence.

Use `node agent-os/scripts/alpha-release-readiness.mjs --strict` as the final local blocker check before asking a human to approve a tag, signature, publication, or announcement.
`node agent-os/scripts/alpha-release-evidence.mjs --json` embeds a sanitized readiness projection so release bundles expose blocker ids without storing local command output.
The readiness report includes Git metadata write access because autonomy can validate code successfully while still being unable to stage or commit in a restricted local checkout.
It also includes the autonomy handoff toolchain so commit-blocked sessions can prove their tracked patch, untracked file contents, restore plan, and tamper checks are internally consistent before another environment resumes the release work.
It includes unsigned review artifact validation so release preparation can prove task review evidence can be generated, verified, and rejected when tampered before any future signing layer is introduced.
Source install evidence follows `docs/source-install.md`; it is a local source-built install path, not a signed binary distribution.

## Optional Live Gate

Run live validation when the host has the required services and binaries:

```bash
cd agent-os
cargo test --workspace --exclude covenant-settlement-program -- --ignored live_
```

Record unavailable prerequisites explicitly. Linux gVisor evidence follows `docs/gvisor-live-runner.md`; model-backed tests require the configured local or external model provider described by the test.

## Human-Owned Decisions

The following decisions remain human-owned until the project has explicit automation policy and credentials:

- release id and tag name;
- whether the alpha candidate should be published;
- project signing key custody;
- key rotation and revocation;
- artifact upload destinations;
- public release announcement language.

Automation may prepare evidence, validate the repository, draft release notes, and flag blockers. It must not tag, sign, publish, or announce a release without explicit authorization.

## Alpha Exit Criteria

The source alpha boundary can be tightened when:

- release evidence bundles are generated routinely;
- release artifact subject schemas are implemented;
- project signing custody is decided and automated through a neutral project identity;
- signed release attestations verify in CI;
- live sandbox validation runs on a reproducible Linux host;
- package distribution paths and upgrade policy are documented and tested.
