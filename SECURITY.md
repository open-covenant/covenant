# Security Policy

Covenant treats anything that can move value, alter execution rights on the daemon, or affect identity / capability / settlement integrity as in scope for responsible disclosure.

## Supported versions

Pre-alpha: the latest `main` branch is in scope.

Post-launch: the latest released daemon and on-chain settlement program remain in scope, plus the immediately previous release during the rollout window.

## Reporting a vulnerability

Do not open a public issue for anything that could compromise keys, capability tokens, on-chain funds, audit-log integrity, or the daemon's enforcement boundary.

Preferred channels:

1. **GitHub private advisory:** [github.com/open-covenant/covenant/security/advisories/new](https://github.com/open-covenant/covenant/security/advisories/new)
2. **Email:** [security@opencovenant.org](mailto:security@opencovenant.org)

Include:

- affected crate, binary, RPC route, or program
- impact and realistic attacker outcome
- minimal reproduction (commands, payloads, or transaction sequence)
- suggested mitigation, if you have one

We aim to acknowledge within 48 hours and share an initial triage decision within 7 days.

## Scope

### In scope

- daemon enforcement paths (capability checks, audit logging, agent dispatch)
- identity and key management (`covenant-identity`)
- capability sign / verify (`covenant-permissions`)
- IPC and HTTP gateway authentication paths
- on-chain settlement program (`programs/settlement`)
- agent runtime isolation boundary

### Out of scope

- spelling and copy issues
- log-level / cosmetic bugs in the operator UI
- third-party vulnerabilities that should be reported upstream
- attacks that require prior compromise of the operator's machine or signing key

## Severity

| Severity | Example |
|---|---|
| Critical | unauthorized capability bypass, unsigned dispatch, on-chain fund loss, key extraction |
| High | audit-log forgery, capability replay, settlement bypass |
| Medium | logic flaw without direct loss of value |
| Low | defense-in-depth or hardening issue |

Severity is assigned by maintainers after triage.
