# covenant-skill

Source for the `covenant` Anthropic Agent Skill: instructions that teach a coding
agent to build Solana applications under Covenant's verifiable execution layer —
signed capability gates, hash-chained audit, separately-keyed verifier refutation,
on-chain anchored witness.

The repository is intentionally minimal and follows the upstream
[Anthropic Agent Skills](https://github.com/vercel-labs/skills) format.

## Layout

```
skill/
  SKILL.md          Frontmatter (name, description) + progressive-disclosure body
  references/       Topic references the agent loads on demand
install.sh          Local install into <project>/.claude/skills/covenant
tests/list.sh       Smoke test: `npx skills add . --list`
LICENSE             Apache-2.0
CODEOWNERS          Maintainership
```

## Install

```bash
# From a checkout
./install.sh --project /path/to/your/project

# Or with the upstream skills CLI
npx skills add open-covenant/covenant-skill
```

## Devnet-only

Every example in this skill defaults to Solana devnet. The skill never requests
seed phrases or private keys and never executes remote bootstrap.

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
