# Covenant Guard

Run your coding agent unattended. It can't spend past your cap, can't touch what
you didn't allow, and hands you a signed receipt of everything it did.

```
covguard run --budget 10 -- claude -p "fix the flaky tests" --dangerously-skip-permissions
```

Most people babysit their agents because they don't trust them alone. Covenant
Guard turns "I have to watch this" into "I can walk away." It runs as the parent
process, outside the sandbox the agent lives in — which is the whole point:
that position is what lets it hold the credential, count the spend, and pull the
plug, none of which the agent can reach around.

## What it does

**Caps the spend.** The agent's model calls are routed through a local metering
proxy. Spend is counted as each response streams, and the moment it crosses your
cap the proxy stops forwarding and the guard kills the run. Overshoot is bounded
to the one call in flight. This works whether the agent runs headless or
interactive, and it covers subscription logins — cases a built-in budget flag
doesn't.

**Sandboxes execution.** The agent runs under an OS sandbox: it can write to the
workspace and nowhere else, its own configuration is read-only so it can't
rewire the guard away, key material is unreadable, and the only network path
open is the metering proxy. That last part is what makes the cap real — there is
no route to the API that skips the meter.

**Hands back a receipt.** Every step is recorded on a hash chain. When the run
ends you get a signed receipt: what the agent spent against your cap, the files
it changed, the models and tokens, the commands it ran — verifiable after the
fact. Tamper with any number and `covguard verify` rejects it.

## Install

```
curl -fsSL https://opencovenant.org/guard/install.sh | sh
covguard doctor
```

Also available via `brew install open-covenant/tap/covenant-guard` and
`cargo install covenant-guard`. macOS today; Linux is close behind.

## Commands

| Command | What it does |
|---|---|
| `covguard run --budget N -- <agent> [args]` | Run the agent under the cap, the sandbox, and the receipt |
| `covguard verify <receipt.json>` | Re-check a receipt's signature and event chain |
| `covguard receipts [list \| show \| open]` | Browse past runs |
| `covguard card <id\|last> [--png out.png]` | Render the shareable receipt card |
| `covguard doctor` | Check the environment is ready |

Defaults: `--budget 10.00`, a 12-hour wall-clock limit. Nothing is sent
anywhere; state lives in `~/.covenant-guard`.

## What it is not

Not a payments system, not an identity or reputation layer, not a place to keep
secrets. One sharp capability: let the agent run, and prove it stayed inside the
lines. Open source under Apache-2.0.
