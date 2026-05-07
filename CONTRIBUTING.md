# Contributing to Covenant

Thanks for the interest. Covenant is operating-layer infrastructure — daemon, runtime, identity, permissions, settlement — and we treat behavior on those surfaces with PR-grade review discipline.

## Before you start

- Read the [README](./README.md) for the current shape of the project.
- Check open issues for context — opening a small issue before a non-trivial change is welcome.

## Development setup

Prerequisites: Rust (stable), Node.js 22+, pnpm 10+. The Solana program build also wants Anchor and `solana-cli`; everything else builds without it.

```bash
git clone git@github.com:open-covenant/covenant.git
cd covenant
```

Common checks (run from the active build root):

- `cargo check  --workspace --exclude covenant-settlement-program`
- `cargo test   --workspace --exclude covenant-settlement-program`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

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

## Submitting changes

- Run the relevant checks before pushing.
- Keep scope tight — one intent per commit.
- Include tests on changed surfaces.
- Update docs and metadata in the same change when public behavior shifts.
- Pull requests are welcome for external contributions, risky changes, or anywhere an async review trail is useful. Direct pushes to `main` are the maintainer default.

## Reporting bugs

- **Security-sensitive:** follow [SECURITY.md](./SECURITY.md). Do not open a public issue.
- **Everything else:** open an issue with reproduction steps, the affected commit or release, and relevant logs.

## License

By contributing, you agree your contributions will be licensed under the [Apache License 2.0](./LICENSE).
