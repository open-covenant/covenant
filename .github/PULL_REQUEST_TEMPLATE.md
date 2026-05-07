<!-- Optional workflow: maintainers usually push directly to main. Use this template when a PR is the right collaboration artifact. -->

## Summary

<!-- One paragraph: what changed and why. Reference the issue if relevant. -->

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation only
- [ ] Infrastructure / tooling

## Contributor declaration

- [ ] I have described the work honestly and included known tradeoffs, limitations, and follow-up risks.
- [ ] I did not include secrets, private keys, capability tokens, or non-public operational details.
- [ ] I disclosed AI-assisted contributions where they materially influenced generated code, tests, or prose.
- [ ] I confirm this change is intended for public release under the repository license.

## On-chain checklist (required if `programs/` changed)

- [ ] Implementation matches the agreed shape
- [ ] Unit tests cover happy path + every documented failure mode
- [ ] `anchor build` and `anchor test` pass locally
- [ ] No unchecked authority or settlement path was introduced
- [ ] Events emitted for every state transition
- [ ] IDL change is reviewed for backward compatibility

## Test plan

<!-- How a reviewer can validate this locally. Commands, not prose. -->

- [ ] `cargo build  --workspace --exclude covenant-settlement-program`
- [ ] `cargo test   --workspace --exclude covenant-settlement-program`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --check`
- [ ] `pnpm --dir landing build` (if `landing/` changed)
- [ ] `anchor build` (if `programs/` changed)
- [ ] Additional checks listed below, or not applicable with rationale.

## Security review

- [ ] No identity, capability, audit, or settlement boundary changed.
- [ ] Security-sensitive changes include failure-mode tests and reviewer notes.
- [ ] New dependencies, GitHub Actions, and external services were reviewed for supply-chain risk.

## Related

<!-- Issues, prior PRs, security findings. -->
