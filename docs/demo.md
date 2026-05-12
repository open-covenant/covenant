# Demo: a single round trip through the daemon

A scripted walkthrough of one intent end-to-end. Each block shows the command, the expected output (abbreviated), and the surrounding artifacts the daemon produced.

Prerequisites: workspace built (`cd agent-os && cargo build --workspace --exclude covenant-settlement-program`) and the binaries on `PATH` or referenced relatively. `$COVENANT_HOME` defaults to `~/.covenant`.

## 1. Register the sample agent

The daemon loads agents from `$COVENANT_HOME/agents/` at startup. Copy the example in:

```bash
mkdir -p ~/.covenant/agents
cp -R ./examples/hello-agent ~/.covenant/agents/hello
```

The manifest is parsed and validated by `covenant-manifest`. Invalid manifests (missing fields, sandbox-required without a sandbox-grade backend, unknown runtime) fail startup rather than silently downgrading.

## 2. Start the daemon

```bash
./agent-os/target/debug/covenantd
```

```
covenantd 0.1.0 listening on $COVENANT_HOME/sock
runtime backend: trusted-local
audit log:       $COVENANT_HOME/audit/events.jsonl
registered:      1 agent (hello@0.1.0)
```

## 3. Dispatch an intent

```bash
./agent-os/target/debug/covenant intent "say hello"
```

```
hello — you asked: 'say hello'
```

What the daemon did:

1. Normalized the request into a typed `Intent`.
2. Routed it to the `hello` agent.
3. Validated the agent's `intent.subscribe` capability against the daemon's identity registry.
4. Dispatched the agent under `trusted-local` (one bounded subprocess, wall-clock timeout enforced).
5. Captured stdout and returned the JSON payload's `text` to the caller.
6. Wrote one `IntentDispatched` audit row and appended a chain entry.

## 4. Inspect the audit row

```bash
./agent-os/target/debug/covenant audit recent --kind IntentDispatched --json | tail -1
```

```json
{
  "id": "1f3c...",
  "timestamp_ms": 1747035072000,
  "kind": {
    "IntentDispatched": {
      "intent_id": "...",
      "agent_id": "hello",
      "subject": "operator@local",
      "duration_ms": 38
    }
  }
}
```

## 5. Verify the audit chain

```bash
./agent-os/target/debug/covenant audit verify --json
```

```json
{
  "events_checked": 4,
  "chain_valid": true,
  "head_chain_hash_hex": "9a2e8b...",
  "errors": []
}
```

The chain links each retained row to the next via SHA-256 (see [audit-integrity.md](./audit-integrity.md)). Any modification to the retained log that is not accompanied by a rewrite of every downstream chain row produces a verification failure.

## 6. Inspect the local settlement receipt

```bash
./agent-os/target/debug/covenant chain receipts read --json | tail -1
```

```json
{
  "id": "rcpt-...",
  "payer": "hello@local",
  "resource": "Compute",
  "credits_consumed": 1,
  "settled_at": 1747035072000
}
```

One receipt was produced for the compute consumed by the agent subprocess. No on-chain settlement is wired; the `onchain_sig` field is `null` (see [the paper](../paper/main.pdf), §10, on the scaffolded settlement model).

## What this exercised

| Primitive   | Touched here | Not touched here                                |
|-------------|--------------|--------------------------------------------------|
| Intent      | ✓            |                                                  |
| Runtime     | ✓ (trusted-local) | gVisor backend, sandbox-required dispatch  |
| Memory      | —            | tier writes, drift checks, repair, compaction    |
| Identity    | ✓            | peer registry, peer revocation                   |
| Permissions | ✓            | scope predicates beyond `intent.subscribe`       |
| Comms       | ✓ (CLI → IPC)| MCP adapter, A2A mailbox                         |
| Compositor  | —            | operator console at `agent-os/covenant-web`      |
| Settlement  | ✓ (local receipt) | on-chain burn, treasury, provider payout    |

Add a network-using tool to the agent manifest and re-run to exercise MCP and the `tool.call.<name>` predicate. Add a sandbox section with `required = true, backend = "linux-gvisor"` to exercise the fail-closed dispatch invariant.
