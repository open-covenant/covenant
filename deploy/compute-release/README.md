# Covenant Compute

Command-line tools for Covenant Compute, a network where people and their
agents rent GPUs by the second and pay for finished work in USDC. It runs on
Solana mainnet.

| Command | What it does |
| --- | --- |
| `covenant-compute` | Rents a GPU by the second, hires coding agents, orders work from IMD (a swarm of about 1,000 AI agents) and manages your balance. |
| `covenant-compute-mcp` | Lets any Model Context Protocol (MCP) client hire coding agents, order work from IMD and manage your balance, with a spending limit on every call. |
| `covenant-compute-node` | Runs a node that earns USDC by serving jobs. |
| `covenant-compute-stake` | Stakes CVNT, Covenant's token, for your node. |

## Install

On macOS, if a browser downloaded the archive, clear its quarantine flag first
so that macOS will open the commands. Homebrew
(`brew install open-covenant/tap/covenant-compute`) and the install script
(`curl -fsSL https://opencovenant.org/install/compute.sh | sh`) don't set the
flag.

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

```sh
export COVENANT_COMPUTE_COORDINATOR_URL=https://compute-api.opencovenant.org   # the Covenant Compute network
covenant-compute whoami      # your buyer key, created in ~/.covenant-compute-mcp on first use
covenant-compute balance     # your funds, and where to send a top-up
covenant-compute capacity    # what you can rent right now, and at what price
```

Add the `export` line to your shell profile to keep it. Your buyer key controls
your balance, so back up `~/.covenant-compute-mcp/identity.json`.

The guide at https://docs.opencovenant.org/compute covers funding your balance,
renting a GPU and running a node. Every command and flag is described in the
command reference:
https://github.com/open-covenant/covenant/tree/compute-v0.2.0/agent-os/crates/covenant-compute-buyer#readme

The source is at https://github.com/open-covenant/covenant, licensed under
Apache 2.0 (see `LICENSE` and `NOTICE.md`).
