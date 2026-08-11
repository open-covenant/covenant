# Daemon-driven settlement flush (devnet)

**Devnet only. Opt-in, default off. Not production.** This is the mechanism
that anchors internal settlement receipts on-chain without an operator in the
loop. It is proven end to end on devnet; promoting it to mainnet is a
separate, human-approved step, and the driver refuses to run against anything
but devnet. See [BUILT.md](../BUILT.md) for the honesty boundary — the
daemon-driven economic lifecycle is not yet production.

## Architecture

`covenant-settlement` records resource receipts to a JSONL log and owns the
`build_receipt_batch` / `mark_batch_confirmed` primitives. On its own nothing
anchors those batches; this driver closes the loop:

1. On an interval the daemon takes the oldest unsettled receipts — up to
   `COVENANT_SETTLEMENT_FLUSH_LIMIT` per tick — and forms a batch. Draining
   oldest first means a backlog larger than one batch is worked in order
   across successive flushes, never stranded outside a newest-N window. The
   `batch_id` is derived from the receipts' merkle root, so the same receipts
   always rebuild the same batch.
2. It hands the batch to the standalone `covenant-settlement-signer` sidecar —
   the same subprocess isolation the x402 and Metaplex signers use. The
   anchoring key and the anchor-lang / solana-sdk dependency tree stay out of
   the daemon's address space and build.
3. The sidecar submits `anchor_receipt_batch` to the deployed settlement
   program, waits for on-chain confirmation, and returns the signature, slot,
   and block time.
4. Only after confirmed finality does the daemon call `mark_batch_confirmed`
   with the real signature. An unconfirmed or failed submission leaves the
   receipts unsettled and retryable — never stranded as "settled" against a
   transaction that never landed.

Idempotency is structural: `anchor_receipt_batch` inits the batch PDA, so a
second submission of the same `batch_id` cannot succeed. The sidecar preflights
`getAccountInfo` on the batch PDA and, if it already exists with a matching
root, reports the original signature instead of resubmitting. A crash between
anchor and mark leaves the receipts unsettled; the next tick rebuilds the same
batch id and takes the already-anchored path.

## Devnet safety

The driver is gated on both ends — the daemon before it spawns a signer and the
sidecar before it loads the key or opens a socket:

- the cluster must be exactly `devnet`;
- the RPC URL must name devnet and must **not** name mainnet (two-sided, fail
  closed);
- the configured program id must equal the program the sidecar is linked
  against;
- the endpoint's live genesis hash (`getGenesisHash`) must be devnet's — the
  authoritative check, so an RPC URL that merely looks like devnet but actually
  fronts another cluster is refused before the key is loaded.

The anchoring keypair is read only inside the sidecar; the daemon holds its
path, never its bytes. Mainnet promotion is out of scope here and hard-gated:
do not point this at mainnet or load a mainnet-controlling key.

## Configuration

```sh
COVENANT_SETTLEMENT_AUTO_FLUSH=false            # enable the driver (default off)
COVENANT_SETTLEMENT_CLUSTER=devnet              # must be devnet
COVENANT_SETTLEMENT_RPC_URL=                    # devnet RPC; must name devnet, not mainnet
COVENANT_SETTLEMENT_PROGRAM_ID=                 # must equal the linked settlement program id
COVENANT_SETTLEMENT_KEYPAIR=                    # devnet anchoring keypair; read by the sidecar only
COVENANT_SETTLEMENT_SIGNER_BIN=                 # path to covenant-settlement-signer
COVENANT_SETTLEMENT_FLUSH_INTERVAL_SECS=900     # flush cadence
COVENANT_SETTLEMENT_FLUSH_LIMIT=256             # max receipts per flush batch (oldest drained first)
```

## Verifying on devnet

The `live_settlement_devnet_flush` integration test drives the whole cycle
against a real devnet program with a funded key that is the program's config
authority. It is `#[ignore]`'d and skips when its environment is unset:

```sh
cargo build -p covenant-settlement-signer   # from agent-os/crates/covenant-settlement-signer
COVENANT_SETTLEMENT_SIGNER_BIN=/abs/path/covenant-settlement-signer \
COVENANT_SETTLEMENT_KEYPAIR=/abs/path/devnet-settlement-signer.json \
COVENANT_SETTLEMENT_RPC_URL=https://api.devnet.solana.com \
COVENANT_SETTLEMENT_PROGRAM_ID=<settlement program id> \
cargo test -p covenantd --test live_settlement_devnet_flush -- --ignored --nocapture
```

It seeds one receipt, flushes it to a confirmed on-chain anchor, asserts the
receipt is settled with the real signature/slot/timestamp, then reflushes to
confirm a settled store has nothing left to anchor.

## Known limitations

Acceptable for a devnet, opt-in mechanism; tracked here for the
mainnet-promotion step:

- **Full-log scan per flush.** Settled and unsettled receipts share one
  append-only log, so each tick reads the whole log to find the oldest
  unsettled batch and `mark_batch_confirmed` rewrites the whole file — O(total
  receipts) per flush. A long-lived, high-volume deployment needs a compacting
  or cursor-based store first; the `Settlement::unsettled` default documents the
  early-exit override hook for that.
- **Orphaned batch PDA on a crash mid-flush.** If the daemon crashes between a
  confirmed anchor and `mark_batch_confirmed` and new receipts arrive before
  restart, recovery anchors a superset batch under a new id and leaves the
  original PDA orphaned (wasted devnet rent). No receipt is lost or
  double-settled and no funds move — anchoring records a merkle root only.
