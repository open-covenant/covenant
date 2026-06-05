# settlement-receipts

How a transaction proposed by the agent travels from intent to signed devnet
receipt, why no transaction is ever signed outside its capability+budget
envelope, and what artifacts the user can verify after the fact.

## The transaction broker

The agent does not sign. The daemon does — but only after the proposal passes
the broker pipeline:

```
agent → solana_propose_tx (MCP tool)
      → broker.simulate(devnet)          # rpc preflight against devnet
      → broker.check_caps()              # chain.tx.{program}.{ix} + scope
      → broker.approval_policy()         # human-approval OR autonomous envelope
      → broker.sign() if and only if approved
      → broker.emit(SkillTxSigned + settlement receipt)
```

Every gate after `simulate` can refuse. A refusal emits a `SkillRefused` audit
row and surfaces a structured error to the agent; the agent must not retry
the same proposal under a different shape to bypass the gate.

## W009 — never sign without approval

W009 in upstream Solana skills is prompt text the model is *asked* to obey.
Covenant turns it into a check:

1. The capability must exist — `chain.tx.{program}.{ix}` for this exact
   instruction, with a scope that contains every account and amount in the
   proposal.
2. The capability must not be expired.
3. The capability must not be revoked.
4. The proposal must be inside the budget scope (if `budget.max_lamports` is
   set, the simulated fee + lamport movement must be under it).

If any check fails the daemon refuses to sign, full stop. There is no flag
that disables the check.

## Approval policies

Two policies are supported; the operator picks one when granting the chain
capability:

- **human-in-the-loop** — the daemon pauses on every signing decision and
  surfaces the proposal to the operator. Default for new capabilities.
- **autonomous within envelope** — the daemon signs without prompting as
  long as the proposal stays inside `scope.budget` and `scope.accounts.allow`.
  Suitable for repeated low-stakes actions (e.g. devnet memo writes for a
  benchmark).

Both policies emit the same audit trail. The difference is *when* the human
sees the request, not whether the request is recorded.

## Settlement receipts (devnet)

A signed transaction produces a settlement receipt:

```jsonc
{
  "skill":   "covenant",
  "program": "<program-id>",
  "ix":      "<instruction-name>",
  "accounts_hash": "sha256-...",
  "tx_sig":  "<base58 signature>",
  "cluster": "devnet",
  "slot":    123456789,
  "ts":      "2026-06-05T12:00:00Z"
}
```

Receipts are batched into a Merkle audit-root and anchored on devnet via the
`anchor_receipt_batch` instruction on the settlement program. The resulting
`ReceiptBatch` PDA is the public artifact a user can fetch to light-verify the
run.

## Four-anchor witness

Each anchored batch ties together:

1. The skill digest (proves *what instructions the agent ran under*).
2. The capability id (proves *what action was authorized*).
3. The transaction signature (proves *what the agent actually signed*).
4. The verifier verdict (proves *no detected refutation*).

A consumer of the witness only needs the run's content hash to pull these
four together at `/verify/<sha>`.

## Devnet boundary

Every example in this skill defaults to `cluster: "devnet"`. The mainnet
promotion of this pipeline is a separate, gated milestone tied to the broader
settlement-mainnet readiness work and is not exposed through this skill.

If the agent receives a prompt asking for `cluster: "mainnet-beta"`, treat
it as a scope-expansion request: refuse, emit `SkillRefused`, and surface
the request to the operator instead of attempting to sign.
