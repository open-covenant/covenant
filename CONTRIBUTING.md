# Contributing to Covenant

Thanks for the interest. Covenant is operating-layer infrastructure: daemon, runtime, identity, permissions, and settlement. We treat behavior on those surfaces with PR-grade review discipline.

## Before you start

- Read the [README](./README.md) for the current shape of the project.
- Check open issues for context. Opening a small issue before a non-trivial change is welcome.

## Development Setup

Prerequisites: Rust (stable), Node.js 22+, pnpm 10+. The Solana program build also wants Anchor and `solana-cli`; everything else builds without it.

```bash
git clone git@github.com:open-covenant/covenant.git
cd covenant
```

Common checks:

- `bash agent-os/scripts/validate.sh --scripts` for repo guardrails without Rust tooling.
- `bash agent-os/scripts/validate.sh --quick` for local iteration.
- `bash agent-os/scripts/validate.sh` before integration.
- `pnpm --dir landing build` when public docs or the site changed.

For the landing site:

- `pnpm --dir landing install --frozen-lockfile --ignore-workspace`
- `pnpm --dir landing build`

## Code style

- Rust: `cargo fmt` + `cargo clippy -- -D warnings`. Prefer early returns over nesting; flat over abstract.
- TypeScript: Prettier defaults; match the surrounding file's conventions.
- Match established patterns before introducing new abstractions.
- No filler: dead code, placeholder TODOs without owners, and AI-narration comments are out.

## Tests

- Unit tests live next to the code they exercise.
- Integration tests against test doubles are the default.
- Tests prefixed `live_` exercise real backends (real network, real subprocess, real model). They are `#[ignore]`'d to keep CI fast and run with `cargo test -- --ignored live_`.
- Adding `live_` coverage when changing protocol-bearing surfaces is strongly preferred.

## Autonomous Workflow

This repository accepts agent-assisted contributions, but the output is held to the same standard as any other change. Read [docs/autonomous-development.md](./docs/autonomous-development.md) before making broad changes.

At minimum:

- Plan before editing when more than one credible architecture path exists.
- Run the security gate before integrating changes to identity, permissions, audit, settlement, sandboxing, secrets, CI, or release automation.
- Add failure-mode tests for new public behavior.
- Update docs and roadmap status when public behavior or claims change.
- Record human-only blockers instead of working around missing credentials or authority.

## Submitting changes

- Run the relevant checks before pushing.
- Keep scope tight: one intent per commit.
- Include tests on changed surfaces.
- Update docs and metadata in the same change when public behavior shifts.
- Pull requests are welcome for external contributions, risky changes, or anywhere an async review trail is useful. Direct pushes to `main` are the maintainer default.

## Reporting bugs

- **Security-sensitive:** follow [SECURITY.md](./SECURITY.md). Do not open a public issue.
- **Everything else:** open an issue with reproduction steps, the affected commit or release, and relevant logs.

## License

By contributing, you agree your contributions will be licensed under the [Apache License 2.0](./LICENSE).
