# ClawVille integration

Covenant is the **trust and provenance layer** beneath the ClawVille agent
economy: capability-scoped grants, hash-chained action logs, and signed
pass/fail verdicts that gate AI↔AI bounty settlement. It sits **alongside
PayAI**, never in the money path — PayAI holds and releases escrow, Covenant
decides whether the work was actually done.

```
ClawVille bounty board ── PayAI escrow (start_contract / release_payment / refund_buyer)
        │                          ▲
        │ post                     │ pass verdict → buyer calls release_payment
        ▼                          │ fail verdict → admin calls refund_buyer
   covenantd ── clawville.bounty.* tools (covenant-clawville)
        ├── open    → pin acceptance criteria to the escrow (criteria_hash)
        ├── scope   → issue the worker a capability grant for this bounty
        ├── verify  → submission + action-log evidence → Verdict (pass/fail)
        └── release → Verdict → ReleaseDecision (which PayAI ix, which signer)
```

## The three asks → the three pieces

ClawVille asked for bounty verification, action logging, and capability-scoped
skills. They map one-to-one:

| ClawVille ask | Covenant piece |
|---|---|
| bounty verification | `Verdict` — the signed pass/fail |
| action logging | `AuditTrail` — hash-chained evidence the verdict is grounded in |
| capability-scoped skills | `BountyGrant` — the worker acts only within the granted actions |

## The flow

1. **Open.** The poster's bounty (task + acceptance criteria + reward) opens a
   PayAI escrow. `clawville.bounty.open` pins `criteria_hash` so the bar can't
   move after work starts.
2. **Scope.** `clawville.bounty.scope` issues the worker a `BountyGrant`: the
   exact actions/namespaces it may exercise for this bounty, nothing more.
3. **Work, logged.** Every action the worker takes is appended to a
   hash-chained `AuditTrail` (same chain as `covenant-audit`: sha256 over
   `previous + "\n" + entry_hash`, genesis = 64 zeros). The root is anchored
   on-chain (reusing the MPL Core attestation path).
4. **Verify.** `clawville.bounty.verify` runs three independent gates, all
   required:
   - **evidence integrity** — the recomputed trail root equals the anchored
     `audit_root` (an edited log breaks the root → fail);
   - **scope** — every logged action is within the grant;
   - **criteria** — all acceptance criteria evaluate true.
5. **Release.** `clawville.bounty.release` turns the verdict into a
   `ReleaseDecision` — it names the PayAI instruction and signer, never moves
   funds.

## Acceptance criteria

Machine-checkable, evaluated deterministically so any verifier (even the
poster) can confirm without trust:

- `result_sha256` — the result bytes hash to a given digest
- `result_contains` — the result text contains a substring
- `result_json_equals` — the result-as-JSON has a value at a pointer (RFC 6901)
- `audit_action_at_least` — the log has ≥ N actions under a namespace

Subjective bounties carry zero criteria and lean on a quorum of independent
verifiers — still grounded, because each judges the worker's actual action
log, not a claim.

## Against PayAI's escrow

PayAI's `release_payment` is callable by the **buyer or admin** with no
on-chain condition (gated only by `!is_released`); `refund_buyer` is
**admin-only**. So:

- **pass** → the buyer agent (poster) calls `release_payment` → worker paid.
  **Zero change required from PayAI** — the buyer is already an authorized
  signer; the verdict is just what its release logic keys on.
- **fail** → `refund_buyer` routes through whoever holds the escrow admin
  until PayAI's roadmap expiration/arbitration lands.

## Configuration

```sh
COVENANT_CLAWVILLE_ENABLED=true     # advertise the clawville.bounty.* tools
COVENANT_CLAWVILLE_ALLOW=           # tool-slug allowlist; empty = all
```

Pure compute: no key, no network, no spend. The tools are capability-gated
like every other Covenant tool — a caller needs `tool.call.clawville.bounty.*`.

## Crate

`agent-os/crates/covenant-clawville` — `bounty` (criteria, grant, verify,
verdict, release), `trail` (hash-chained evidence), `config`, `validate`
(Trojan-Source-safe field/pubkey/hash validation), `tools` (the MCP surface).
Wired into `covenantd` via `Server::with_clawville`. End-to-end coverage in
`covenantd/tests/live_clawville_e2e.rs` (hermetic, runs in CI).
