# Security Policy

## Reporting a vulnerability

Email **security@opencovenant.org** with details — please do not open a public
issue for security reports. We aim to acknowledge within 72 hours.

## Safety model

The `covenant` skill is **devnet-first** and never requests seed phrases, secret
recovery phrases, private keys, or keystore files. Signing keys are owned by the
Covenant daemon; the agent never handles raw signing material. On-chain account
data is treated as untrusted input. Mainnet promotion is a separate, gated
milestone outside this skill's scope.

## Scope

In scope: the `covenant` skill in this repository — `skill/SKILL.md` and
`skill/references/`.
