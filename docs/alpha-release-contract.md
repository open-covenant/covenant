# Alpha Release Contract

Covenant alpha releases are source-built local infrastructure releases. They are intended to give systems engineers and agent-infrastructure contributors a reproducible local control plane to inspect, build, run, and extend. They are not binary distribution releases, SDK stability commitments, production security certifications, or public signing events.

The alpha boundary exists to keep public release language aligned with implementation evidence.

## Release Boundary

An alpha candidate may claim:

- the repository builds from source on supported developer hosts;
- the local daemon, CLI, IPC, HTTP gateway, memory, audit, identity, permissions, peer auth, A2A, budget, and local receipt surfaces are covered by the documented validation profile;
- live tests exist for selected real process, CLI, daemon, HTTP, model, A2A, MCP, and sandbox boundaries where prerequisites are available;
- provenance envelopes and audit-root attestations provide local consistency evidence;
- distributed settlement, installers, SDK publication, signed release artifacts, and transparency-log publication remain planned work.

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

- `evidence.json`: output from `node agent-os/scripts/alpha-release-evidence.mjs --json`;
- validation notes listing each required command, outcome, host assumptions, and skipped live prerequisites;
- links to any provenance envelopes or audit-root attestations generated for the candidate;
- the release decision: draft, accepted, rejected, or superseded.

The evidence helper is read-only. It records the commit, branch, dirty-file count, recommended commands, and release notes. It does not execute the commands and does not create a tag or artifact.

## Minimum Local Gate

Run from the repository root:

```bash
node agent-os/scripts/alpha-release-evidence.mjs --json
bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/validate-autonomy.mjs
node agent-os/scripts/validate-live-coverage.mjs
node agent-os/scripts/validate-git-identity.mjs
node agent-os/scripts/validate-readme-copy.mjs
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check
```

A release candidate should have a clean working tree before evidence is accepted. Dirty output from the evidence helper is useful while preparing a release, but it is not release evidence.

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
- installer and package distribution paths are documented and tested.
