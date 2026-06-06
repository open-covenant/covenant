# covenant-witness

How a separately-keyed verifier refutes runs that crossed a trust boundary, and
how the result is anchored on devnet so anyone can light-verify a run.

## Separately-keyed verifier

The verifier-refuter holds a *different* keypair from the agent. It reads the
audit log and emits a signed verdict per run. The verdict is a refutation when
the chain shows:

1. A run signed a transaction whose required `chain.tx.{program}.{ix}` grant was
   missing, expired, or scope-violated.
2. A run signed a transaction whose causal predecessors include an
   `UntrustedInputObserved` row carrying injected-instruction content (the W011
   boundary — see [covenant-audit](covenant-audit.md)).
3. The audit-chain hash is broken (file tampering).

A run with no refutation is *verifiable*, not "trusted" — the user can recompute
the audit chain themselves and reach the same verdict. The verdict is signed by
a key the agent never holds, so the agent cannot forge its own acquittal.

## Devnet witness anchor

Skill events fold into the daemon's Merkle audit-root. The root is anchored on
devnet via the `anchor_receipt_batch` instruction on the settlement program,
producing a `ReceiptBatch` PDA the public can light-verify. The full chain is:

```
covenantd JSONL → audit-root (Merkle)
              → settlement.ReceiptBatch PDA on devnet
              → verifier signed verdict per run
```

## Four-anchor witness

Each anchored batch ties together:

1. The skill digest (proves *what instructions the agent ran under*).
2. The capability id (proves *what action was authorized*).
3. The transaction signature (proves *what the agent actually signed*).
4. The verifier verdict (proves *no detected refutation*).

A consumer of the witness only needs the run's content hash to pull these four
together. The landing UI surfaces this at `/verify/<sha>` for any commit-scoped
or skill-driven run — same UI patterns as the rest of Covenant's witness work.

## What this means for the agent

The agent cannot talk its way out of a refutation. Acting cleanly — refusing
untrusted input, staying inside granted scope, never seeking a private key — is
the only path to a verifiable run. The verdict is computed from the chain, not
from the agent's account of events.
