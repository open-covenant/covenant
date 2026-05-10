# SDK Compatibility

Covenant currently treats the TypeScript SDK packages as workspace-alpha surfaces. They are useful for local apps and integration tests, but they are not public npm stability commitments.

Run the compatibility report from the repository root:

```bash
node agent-os/scripts/sdk-compatibility.mjs --json
```

Validate the report contract:

```bash
node agent-os/scripts/validate-sdk-compatibility.mjs
```

The report uses schema `covenant.sdk-compatibility.v1`.

## Current Packages

| Package | Current status | Compatibility boundary |
|---|---|---|
| `@covenant/sdk` | Workspace alpha, not published to npm | Root export map, TypeScript declarations, and `packages/sdk/compatibility/exports.v1.json` are validated. |
| `@covenant/sdk-ui` | Private workspace alpha | React hooks remain private; `packages/sdk-ui/compatibility/exports.v1.json` tracks workspace export drift. |

## Workspace-Alpha Rules

- Root export maps must stay aligned with `main` and `types` package metadata.
- Source exports should not be removed or renamed without updating the package export fixture, compatibility note, and distribution readiness evidence.
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
