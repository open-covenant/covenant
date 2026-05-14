# Demo: a single round trip through the daemon

A scripted walkthrough of one task end-to-end. Each block shows the command, the expected output (abbreviated), and the surrounding artifacts the daemon produced.

Prerequisites: workspace built (`cd agent-os && cargo build --workspace --exclude covenant-settlement-program --release`) and the binaries on `PATH` or referenced relatively. `$COVENANT_HOME` defaults to `~/.covenant`.

## 1. Register the sample agent

The daemon loads agents from `$COVENANT_HOME/agents/` at startup. Copy the example in:

```bash
mkdir -p ~/.covenant/agents
cp -R ./examples/hello-agent ~/.covenant/agents/hello
```

The manifest is parsed and validated by `covenant-manifest`. Invalid manifests (missing fields, sandbox-required without a sandbox-grade backend, unknown runtime) fail startup rather than silently downgrading.

## 2. Start the daemon

```bash
./agent-os/target/release/covenantd
```

```
covenantd listening path=$COVENANT_HOME/sock
runtime runner ready backend=trusted-local hermes=false
agents loaded agents_dir=$COVENANT_HOME/agents registered=1
```

## 3. Bootstrap capabilities

```bash
./agent-os/target/release/covenant bootstrap
```

```
granted 2 of 2 capabilities to user@local:
  + receive your tasks (intent.subscribe)
  + save to memory (memory.write)

ready. try: covenant intent "say hello"
```

`bootstrap` walks every loaded agent's manifest, takes the union of their required capabilities, adds `memory.write` (the daemon writes a working-memory record on every successful dispatch), and grants the lot to `user@local`. Re-running it is a no-op.

## 4. Dispatch the task

```bash
./agent-os/target/release/covenant intent "say hello"
```

```
hello — you asked: 'say hello'
```

What the daemon did:

1. Normalized the request into a typed `Intent`.
2. Routed it to the `hello` agent.
3. Validated the agent's `intent.subscribe` capability against the daemon's capability store.
4. Dispatched the agent under `trusted-local` (one bounded subprocess, wall-clock timeout enforced).
5. Captured stdout and returned the JSON payload's `text` to the caller.
6. Wrote one `intent_dispatched` audit row and appended a chain entry.

## 5. Inspect the audit row

```bash
./agent-os/target/release/covenant audit recent -n 5 --json
```

The last row is the `intent_dispatched` event with a `result_hash_hex` you can verify against the chain.

## 6. Verify the audit chain

```bash
./agent-os/target/release/covenant audit verify --json
```

```json
{
  "kind": "audit_integrity",
  "report": {
    "anchors": 8,
    "events": 8,
    "failures": [],
    "root_hash_hex": "...",
    "valid": true
  }
}
```

The chain links each retained row to the next via SHA-256 (see [audit-integrity.md](./audit-integrity.md)). Any modification to the retained log that is not accompanied by a rewrite of every downstream chain row produces a verification failure.

## 7. Inspect the local settlement receipt

First grant the read permission (receipt reads are gated):

```bash
./agent-os/target/release/covenant capabilities grant chain.receipts
./agent-os/target/release/covenant receipts recent -n 5 --json
```

```json
{
  "kind": "receipt_list",
  "limit": 5,
  "since_ms": null,
  "receipts": [
    {
      "id": "8b7c45fb-7920-41ce-8c80-bf6b34a2a55c",
      "payer": { "display": "hello@local", "pubkey": "..." },
      "resource": "compute",
      "credits_consumed": 1,
      "settled_at": 1747035072000,
      "onchain_sig": null
    }
  ]
}
```

One receipt was produced for the compute consumed by the agent subprocess. No on-chain settlement is wired; the `onchain_sig` field is `null` (see [the paper](../paper/main.pdf), §10, on the scaffolded settlement model).

## What this exercised

| Primitive   | Touched here | Not touched here                                |
|-------------|--------------|--------------------------------------------------|
| Intent      | ✓            |                                                  |
| Runtime     | ✓ (trusted-local) | gVisor backend, sandbox-required dispatch  |
| Memory      | ✓ (working write) | tier reads, drift checks, repair, compaction |
| Identity    | ✓            | peer registry, peer revocation                   |
| Permissions | ✓            | scope predicates beyond `intent.subscribe`       |
| Comms       | ✓ (CLI → IPC)| MCP adapter, A2A mailbox                         |
| Compositor  | —            | operator console at `agent-os/covenant-web`      |
| Settlement  | ✓ (local receipt) | on-chain burn, treasury, provider payout    |

Add a network-using tool to the agent manifest and re-run to exercise MCP and the `tool.call.<name>` predicate. Add a sandbox section with `required = true, backend = "linux-gvisor"` to exercise the fail-closed dispatch invariant.
