# Governance

Covenant has two governance surfaces: the repository and the on-chain settlement program. The repository governs code, documentation, release process, and security response. The settlement program governs on-chain parameters and execution rights once it is live.

## Repository governance

### Roles

- **Contributor:** anyone opening an issue or proposing a change.
- **Maintainer:** direct-to-`main` access, release authority, and security triage responsibility.
- **Lead maintainer:** resolves tie-breaks and spec-interpretation disputes when needed.

### Repository flow

- Direct pushes to `main` are the default maintainer workflow.
- Pull requests are optional and used for external contributions, risky changes, or cases where a separate review artifact is useful.
- Anyone pushing to `main` is responsible for running the full check suite first and leaving the default branch green.
- Changes touching identity, capability, audit, settlement, or on-chain code should get a second pair of eyes even when not required.
- Security fixes may follow a quieter path where public discussion would increase risk.

## On-chain governance

### Upgrade authority

The settlement program is deployed to Solana and controlled through a multisig-managed upgrade authority until full on-chain governance timelocks are active.

Any production deployment requires:

1. The change landed on `main`.
2. A reproducible build manifest and IDL digest.
3. A green test suite on the affected crates and program.
4. Multisig approval for execution on the target cluster.
5. Public notice, except where a delay would materially increase user risk.

### Protocol parameters

Once on-chain governance is active, Covenant parameters move on-chain. The governance program and the deployment multisig remain distinct controls — neither silently overrides the other.

## Security-sensitive actions

The following actions require heightened review:

- on-chain program upgrades
- treasury or fee-policy changes
- key rotation for the deployment multisig
- changes to identity, capability, or audit primitives
- gateway or settlement policy changes

## Changing this document

Material changes to governance require maintainer sign-off and advance notice in the repository.
