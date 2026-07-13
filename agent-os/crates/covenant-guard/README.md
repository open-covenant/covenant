# covenant-guard

Run your coding agent unattended. It can't spend past your cap, can't touch
what you didn't allow, and hands you a signed receipt of everything it did.

```
covguard run --budget 10 -- claude -p "fix the flaky tests" --dangerously-skip-permissions
```

`covguard` runs as the parent process, outside the sandbox the agent lives in.
That position is the whole design: it lets the guard hold the credential, meter
the spend, and pull the plug, none of which the agent can reach around.

## Three rings

- **Spend cap, enforced from outside the process.** The agent is pointed at a
  loopback metering proxy (`ANTHROPIC_BASE_URL`). Every model call is forwarded
  and its usage is counted as the response streams. When spend crosses the cap
  the proxy stops forwarding and the guard kills the agent's process group.
  Overshoot is bounded to the one call in flight. This works in headless and
  interactive runs, and covers subscription/OAuth sessions that a built-in budget flag does not.
- **OS sandbox, so a bad command can't wreck the machine.** The agent runs
  under a Seatbelt profile on macOS or bubblewrap on Linux: writes confined to
  the workspace, the agent's own config files read-only (so it can't rewire the
  base URL or disable the guard's hooks), key material unreadable, and all
  network egress denied except the loopback proxy. This is what makes the cap
  unbypassable: there is no route to the API that skips the meter. (Linux needs
  bubblewrap installed.)
- **A signed receipt you can check after the fact.** Every event lands on a
  SHA-256 hash chain; on exit the guard writes a receipt carrying the spend
  against the cap, the files changed, the models and tokens, the chain root,
  and an ed25519 signature. `covguard verify` re-checks it; tamper with any
  number and it fails.

## Commands

```
covguard run [--budget USD] [--wall 12h] [--workspace DIR] -- <agent> [args...]
covguard verify <receipt.json> [--signer <pubkey>]
covguard receipts [list | show <id|last> | open <id|last>]
covguard card [<id>|last] [--png out.png]
covguard mcp
covguard doctor
```

`covguard run` takes more flags (`--host`, `--allow-localhost`, `--auth-token`,
`--json`); see `covguard --help`. `covguard mcp` runs a stdio MCP server exposing
read-only tools (`guard_status`, `guard_receipts`, `guard_verify`) to an agent.

Defaults: `--budget 10.00`, `--wall 12h`. State (signing key, receipts) lives in
`~/.covenant-guard`; override with `COVGUARD_HOME`.

## What it is not

Not a payments system, not an identity or reputation layer, not a place to put
secrets. It is one sharp capability: let the agent run, and prove it stayed
inside the lines.

Apache-2.0.
