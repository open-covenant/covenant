# Covenant Compute

Command-line tools for Covenant Compute, a network where people and their
agents rent GPUs by the second and pay for finished work in USDC. It runs on
Solana mainnet.

| Command | What it does |
| --- | --- |
| `covenant-compute` | Rents a GPU by the second, hires coding agents, manages your balance, and buys research reports and other finished work from [IMD](https://imd.fun), a swarm of about 1,000 AI agents. |
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
this step.

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

The guide at https://docs.opencovenant.org/compute covers funding your balance,
renting a GPU, hiring a coding agent and running a node. Every command and flag
of `covenant-compute` and `covenant-compute-mcp` is described in the command
reference:
https://github.com/open-covenant/covenant/tree/compute-v0.2.0/agent-os/crates/covenant-compute-buyer#readme

The source is at https://github.com/open-covenant/covenant, licensed under
Apache 2.0 (see `LICENSE` and `NOTICE.md`).
