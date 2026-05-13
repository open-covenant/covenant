# SDK Compatibility

Covenant currently treats the TypeScript SDK packages as workspace-only surfaces. They are useful for local apps and integration tests, but they are not public npm stability commitments.

Run the compatibility report from the repository root:

```bash
node agent-os/scripts/sdk-compatibility.mjs --json
```

The report uses schema `covenant.sdk-compatibility.v1`. The report-contract validator is maintained internally.

## Current Packages

| Package | Current status | Compatibility boundary |
|---|---|---|
| `@covenant/sdk` | Workspace-only, not published to npm | Root export map, TypeScript declarations, `packages/sdk/compatibility/exports.v1.json`, and `packages/sdk/compatibility/instructions.v1.json` are validated. |
| `@covenant/sdk-ui` | Private workspace-only | React hooks remain private; `packages/sdk-ui/compatibility/exports.v1.json` tracks workspace export drift. |

## Workspace-Only Rules

- Root export maps must stay aligned with `main` and `types` package metadata.
- Source exports should not be removed or renamed without updating the package export fixture, compatibility note, and distribution readiness evidence.
- Solana instruction descriptor names, account order, and data keys should not change without updating `packages/sdk/compatibility/instructions.v1.json`.
- README install examples must keep the unpublished workspace boundary visible.
- Generated protocol bindings are not public-stable until fixture coverage exists for the generated surface.

## Public SDK Graduation

Public SDK readiness remains blocked until:

- npm publication is approved;
- SDK semantic version support windows are approved;
- generated protocol bindings have compatibility fixtures;
- package release artifacts are covered by the release evidence bundle;
- package rollback or deprecation policy is documented.

Automation may validate metadata and draft compatibility evidence. Humans still own npm publication, public semver commitments, and release announcements.
