# Covenant Compute

Command-line tools for Covenant Compute, a network where people and their
agents rent GPUs by the second and pay for finished work in USDC. It runs on
Solana mainnet.

| Command | What it does |
| --- | --- |
| `covenant-compute` | Rents a GPU by the second, saves and restores encrypted checkpoint files, hires coding agents, manages your balance, and buys research reports and other finished work from [IMD](https://imd.fun), a swarm of about 1,000 AI agents. |
| `covenant-compute-mcp` | Lets any Model Context Protocol (MCP) client hire coding agents, buy work from IMD, and check, top up and withdraw your balance. |
| `covenant-compute-node` | Runs a node that earns USDC for paid work such as GPU rentals and coding-agent tasks. |
| `covenant-compute-stake` | Stakes CVNT, Covenant's token, which a node needs before it can take work. |

Both `covenant-compute` and `covenant-compute-mcp` refuse any single lease, task or purchase that would cost more than your spending cap, which is 1 USDC unless you raise `COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC`.

## Install

If you downloaded the archive in a browser on macOS, clear the quarantine flag
on the unpacked commands so that macOS will open them. To check the archive
first, follow "Verify a download" in the release notes. Installing with
Homebrew (`brew install open-covenant/tap/covenant-compute`) or the install
script (`curl -fsSL https://opencovenant.org/install/compute.sh | sh`) avoids
this step. Those installers use separately pinned releases; check that
`covenant-compute --version` reports `0.5.0` before using this release's lease
commands.

From the unpacked folder:

```sh
xattr -d com.apple.quarantine covenant-compute*   # macOS, browser downloads only
mkdir -p ~/.local/bin
install -m 0755 covenant-compute* ~/.local/bin/
```

If `~/.local/bin` is not on your `PATH`, add
`export PATH="$HOME/.local/bin:$PATH"` to your shell profile, and run it now to
use the commands in this terminal.

## Get started

Point the commands at the Covenant Compute network, then check your key, your
balance and what you can rent:

```sh
export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org
covenant-compute whoami      # your buyer key, created in ~/.covenant-compute-mcp on first use
covenant-compute balance     # your funds, and where to send a top-up
covenant-compute capacity    # what you can rent right now, and at what price
```

Add the `export` line to your shell profile to keep it. Your buyer key controls
your balance, and `covenant-compute` and `covenant-compute-mcp` both use it, so
back up `~/.covenant-compute-mcp/identity.json`.

## Security audit workflow

Since version 0.4.1, the tools bundle Cloudflare's security-audit skill, including its six-phase
workflow, report schema, Node.js validators, and MIT license. Inspect it or install
it in the current repository's agent skill directory without a network connection:

```sh
covenant-compute audit-skill --list
covenant-compute audit-skill --read SKILL.md
mkdir -p .agents/skills
covenant-compute audit-skill --export .agents/skills/covenant-compute-security-audit
```

Export requires a new destination and refuses to overwrite an existing one. In
an MCP client, `compute.audit` with `{}` reads the entrypoint; a `document` field
selects another bundled file. Restart an existing MCP server after upgrading its
binary so clients discover the new tool.

Loading or exporting the skill spends nothing and dispatches no work. The calling
agent coordinates the audit, independent reviews, filesystem access, and required
sandbox. These tools do not execute the workflow or add a remote audit job kind.
The bundle includes Cloudflare's unchanged upstream documents and license at
commit `c1c8a8c1471069fb0e188eeaff69b8e8db6564a8` with a hash manifest. The full guide is:
https://github.com/open-covenant/covenant/blob/compute-v0.5.0/docs/compute-security-audit.md

## Upfront model prices

Version 0.4.0 adds opt-in inference tariffs. Read `capacity` for a currently
offered tariff ID, then request its exact model and output limit:

```sh
covenant-compute infer --tariff-id <tariff-id> --model <model-id> \
  --max-tokens 256 --dry-run --json 'Summarize this paragraph.'
```

The dry run shows the fixed quote without buying work. To purchase it, remove
`--dry-run`; the normal spending cap still applies. The price combines the model's
base charge with the requested maximum output length. A shorter answer does not
reduce the agreed charge. `covenant-compute-mcp` exposes the same choice through
`tariff_id`, `model`, and `max_tokens` on `compute.infer`.

Tariffs require a compatible coordinator and an operator advertising the exact
offer. Installing the client does not activate a tariff on the network. Requests
without a tariff ID retain their existing pricing behavior. The tariff guide is:
https://github.com/open-covenant/covenant/blob/compute-v0.5.0/docs/compute-inference-tariffs.md

## Encrypted checkpoints

Version 0.3.0 adds workspace commands to `covenant-compute`. They require a
workspace-enabled coordinator with accepted private storage. Saving a file does
not rent a machine or transfer your Compute balance.

```sh
covenant-compute workspace create training-checkpoint --id <workspace-uuid>
covenant-compute workspace save <workspace-uuid> checkpoint.tar
covenant-compute workspace restore <workspace-uuid> restored.tar
covenant-compute workspace backup <workspace-uuid> private-workspace-backup.json
```

The buyer encrypts and decrypts locally. Its key, version pin and pending upload
state survive a process restart. After an uncertain save, use
`covenant-compute workspace retry <workspace-uuid>` to reconcile the original
operation. Keep the private backup with your buyer identity: it contains the
workspace encryption key. On another client using the same buyer identity and
coordinator, restore it with `workspace import <private-workspace-backup.json>`.

Restore creates a new local file and refuses to overwrite an existing one.
`--lease <lease-uuid>` additionally checks authorization for that buyer's live
lease; it does not copy plaintext to the machine. Archive directories before
saving and transfer restored files through an authenticated connection. These
commands do not provide automatic directory synchronization or hardware
attestation. `workspace delete <workspace-uuid>` schedules stored ciphertext for
cleanup after outstanding grants expire.

The complete workspace workflow and storage limits are documented at:
https://github.com/open-covenant/covenant/blob/compute-v0.5.0/docs/compute-workspaces.md

The guide at https://docs.opencovenant.org/compute covers funding your balance,
renting a GPU, hiring a coding agent and running a node. Every command and flag
of `covenant-compute` and `covenant-compute-mcp` is described in the command
reference:
https://github.com/open-covenant/covenant/tree/compute-v0.5.0/agent-os/crates/covenant-compute-buyer#readme

The source is at https://github.com/open-covenant/covenant, licensed under
Apache 2.0 (see `LICENSE` and `NOTICE.md`).
