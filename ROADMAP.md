# Roadmap

Covenant is built around eight primitives — intent, runtime, memory, identity, permissions, comms, compositor, settlement — that together form an agent-native operating layer. This roadmap describes the active product surface; specifics shift as work lands.

## Current focus

- Harden the local daemon's enforcement boundary: capability checks, audit log, agent dispatch, ignore list.
- Stabilize the MCP and A2A protocol adapters and their capability gating.
- Tighten the operator surfaces — CLI and web UI — to match the daemon's wire format.

## Near term

- Disk-backed mailbox so daemon restarts don't drop queued agent-to-agent tasks.
- Per-resource budget mid-task save and resume.
- Capability shapes for agent-to-agent send and respond, audited end-to-end.
- End-to-end test coverage for the A2A duplex against a real daemon binary.

## Later

- Agent runtime sandboxing — gVisor on Linux first, Firecracker as a follow-up.
- Solana settlement program — SPL CPIs and Pyth oracle wiring; devnet, then mainnet.
- TUI operator surface alongside the web UI.
- SDKs for agent authors in the major languages.
- Optional Wayland compositor integration.
