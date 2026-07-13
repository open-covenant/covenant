# Bento integration: pre-execution firewall as a screen + reputation signal

Status: phase 1. Screening works end to end via a Node sidecar over Bento's SDK. The on-chain reputation read is wired but inert until Bento publishes its account layout.
Crate: `agent-os/crates/covenant-bento`

## Summary

[Bento Guard](https://bento-1.gitbook.io/bento-docs) is a pre-execution firewall for agents: before a transaction signs, it scores the intent (allow, block, or escalate) and records strikes plus an on-chain reputation. This crate consumes that without rebuilding it, in two layers, both off by default.

- **`bento.screen`** runs Bento's `protect()` over a natural-language intent and returns the verdict. It goes through a Node sidecar running Bento's own SDK; the agent key never reaches the daemon. Fails closed (blocks) on any guard failure or timeout.
- **`bento.reputation`** reads an agent's on-chain Bento standing (strikes, lock state) into a labeled soft signal, alongside Covenant's own audit-derived reputation, never blended into it.

Lane discipline: Covenant never re-scores intent or rebuilds the firewall (Bento owns that), and Bento never signs work-completion or issues capability grants (Covenant owns that).

## Enable it

Everything is off until `COVENANT_BENTO_ENABLED` is truthy. Each tool then needs its own input.

| Variable | For | Effect |
| --- | --- | --- |
| `COVENANT_BENTO_ENABLED` | both | Master switch. Truthy (`1`, `true`, `yes`) turns the provider on. |
| `COVENANT_BENTO_PROTECT_ENABLED` | screen | Registers `bento.screen`. Needs the guard binary below. |
| `COVENANT_BENTO_GUARD_BINARY` | screen | Absolute path to the guard sidecar (`crates/covenant-bento/sidecar/bento-guard.mjs`). |
| `COVENANT_BENTO_KEYPAIR_PATH` | screen | Path to the agent's keypair file; forwarded to the sidecar, never read by the daemon. |
| `COVENANT_BENTO_GUARD_TIMEOUT_MS` | screen | Hard per-screen deadline. Default 10000. |
| `COVENANT_BENTO_PROGRAM_ID` | reputation | Bento's on-chain program id. Registers `bento.reputation`. |
| `COVENANT_BENTO_RPC_URL` | reputation | Solana RPC for the on-chain read. Default devnet. |

Screening, minimal:

```
COVENANT_BENTO_ENABLED=1
COVENANT_BENTO_PROTECT_ENABLED=1
COVENANT_BENTO_GUARD_BINARY=/abs/path/to/bento-guard.mjs
COVENANT_BENTO_KEYPAIR_PATH=/abs/path/to/agent-key
```

The sidecar needs a one-time `npm install`; see `crates/covenant-bento/sidecar/README.md`. A live `protect()` call needs a Bento-registered agent (register at `app.bentoguard.xyz`).

## The tools

`bento.screen` input `{ "intent": "<natural language>", "agentAddress"?: "<base58>" }`. Output is the labeled verdict: `recommendation` (ALLOW/BLOCKED/ESCALATED), `gate` (proceed/block/escalate), `riskScore`, `strike`, `reasoning`, and Bento's deep-link URLs. A guard failure returns a structured `BLOCKED` with `failClosed: true`; the internal error is logged for the operator, not returned to the model.

`bento.reputation` input `{ "pubkey": "<base58>" }`. Output is a labeled soft signal (`clean`, `strikes`, `locked`, plus the raw standing). Until Bento publishes the on-chain layout it returns `{ "status": "pending" }`, and an agent with no Bento history returns `{ "status": "no-record" }`. The read never fabricates a value.

## Trust boundary

A screen result is Bento's firewall verdict, stamped `"bento-screened (third-party firewall verdict)"`, not a Covenant decision; Covenant honors the verdict rather than re-scoring it. A reputation result is stamped `"bento-attested (third-party on-chain), soft signal"` so it is never mistaken for a Covenant-verified fact.

## What's gated

The reputation read is built up to the on-chain decode. The program id is known: `A5vQdPeJH2Yn72RmXHyrFjErUTqPwX83e6of4LBchEbG` on devnet (read from the live relayer at `api.bentoguard.xyz/api/v1/system/relayer`), and the model is pinned (block at 70000 of 100000, max 5 strikes, EMA alpha 0.7). The remaining gap is the per-agent account: only Bento's global config sits on base devnet, so the reputation state lives on their MagicBlock ER. Finishing the read needs the ER RPC plus the agent PDA seeds and account layout. Until those land the tool returns `pending`; the base-chain account fetch underneath is live and tested.

Registration is self-serve at `app.bentoguard.xyz` (no Bento gatekeeper; the relayer already reports 92 registered agents), so a live `protect()` only needs an agent registered there and its key in a file.
