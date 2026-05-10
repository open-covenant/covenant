# Third-Party MCP Fixture Selection Policy

Covenant currently exercises MCP subprocess paths with the in-repo fake server. A real third-party MCP server fixture is not shipping. This document defines the selection policy that any future opt-in fixture must satisfy before it is added to the repository or CI.

The policy is enforced by:

```bash
node agent-os/scripts/validate-mcp-fixture-selection-policy.mjs
```

The validator scans this document for the policy fields below and refuses to pass when any required field is missing. It does not import a third-party fixture, fetch upstream sources, or change CI behavior.

No fixture is approved by this document. Vendor selection, license acceptance, and any network access remain human-owned.

## Default-off Semantics

- A third-party MCP fixture must be opt-in. Default `cargo test` and `bash agent-os/scripts/validate.sh --scripts` runs must not download, build, execute, or depend on any third-party fixture.
- The fixture must be guarded behind an explicit feature flag, ignored test attribute, or environment variable that defaults to off.
- The opt-in switch must be documented next to the test so a reviewer can audit it without grep.

## Provenance Pinning

- The fixture must reference a specific upstream tag or commit hash. Floating branches and `latest` tags are rejected.
- The pinned commit must be reproduced into a digest recorded in the repository (SHA-256 of the upstream archive or git commit SHA-1 with a recorded fetch URL).
- A change to the pinned commit requires a separate review entry in `docs/decisions/` or an equivalent ADR boundary, not a silent bump.

## License Check

- The upstream project license must be one of: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, MPL-2.0, or another license explicitly approved by a human reviewer.
- Mixed-license repositories must record which subdirectory the fixture is sourced from and confirm that subtree's license.
- The license header or LICENSE file from the pinned source must be included next to the vendored copy.

## Reproducible Build Evidence

- The fixture build must be reproducible from the pinned source: deterministic Cargo, npm, or shell build with a checked-in lockfile or vendored dependency tree.
- Build commands must run without a network connection once provenance has been recorded.
- The validator or a sibling validator must be able to recompute the recorded digest from the vendored tree.

## Vendor Allowlist

- The repository keeps an explicit allowlist of approved upstream vendors. The allowlist starts empty.
- A vendor enters the allowlist only after a human review records the project, license, pinned commit, and rationale in `docs/decisions/` or an equivalent ADR boundary.
- The validator may scan this document and the ADR set to confirm that any named fixture in the repository corresponds to an allowlisted vendor.

## Network Access

- Fixture tests must not reach the public network during execution. Any required artifacts must be vendored or pinned by digest.
- If a future fixture needs egress (for example, to a sandboxed echo server), it must run inside the documented gVisor live runner with the egress destination listed in the live coverage matrix.
- Network access additions are human-owned and must not be granted by automation.

## Removal Policy

- A fixture must be removable in a single commit that also removes its vendored source, lockfile entries, validator references, and documentation pointer.
- If the upstream project becomes compromised, abandoned, or relicensed in an incompatible way, the fixture must be removed before the next merge to `main`.
- The policy doc is updated in the same commit that removes a fixture so the policy and the repository state stay aligned.

## Human Authority

- Final fixture selection, vendor allowlist additions, license acceptance, and any external network access remain human-owned.
- Automation may draft policy updates, validator scripts, and candidate ADR entries. It must not import a third-party fixture, modify the vendor allowlist, or enable network egress without an approved human decision.
