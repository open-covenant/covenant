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
- [ ] I disclosed automated or agent-produced artifacts where that context materially affects review.
- [ ] I confirm this change is intended for public release under the repository license.

## Autonomous workflow checklist

- [ ] I planned the change before editing or explained why the path was mechanical.
- [ ] I updated docs/status when public behavior, setup, architecture, or project claims changed.
- [ ] I updated `agent-os/autonomy/tasks` when this changed autonomous backlog state.
- [ ] New public behavior includes failure-mode tests or an explicit tracked gap.
- [ ] Human-only blockers are recorded instead of bypassed.

## Security-sensitive checklist

- [ ] No identity, capability, audit, settlement, sandbox, secret, CI, or release boundary changed.
- [ ] Security-sensitive changes include failure-mode tests and reviewer notes.
- [ ] New dependencies, GitHub Actions, and external services were reviewed for supply-chain risk.

## On-chain checklist (required if `programs/` changed)

- [ ] Implementation matches the agreed shape
- [ ] Unit tests cover happy path + every documented failure mode
- [ ] `anchor build` and `anchor test` pass locally
- [ ] No unchecked authority or settlement path was introduced
- [ ] Events emitted for every state transition
- [ ] IDL change is reviewed for backward compatibility

## Test plan

<!-- How a reviewer can validate this locally. Commands, not prose. -->

- [ ] `bash agent-os/scripts/validate.sh --quick`
- [ ] `bash agent-os/scripts/validate.sh`
- [ ] `pnpm --dir landing build` (if `landing/` changed)
- [ ] `anchor build` (if `programs/` changed)
- [ ] Additional checks listed below, or not applicable with rationale.

## Related

<!-- Issues, prior PRs, security findings. -->
