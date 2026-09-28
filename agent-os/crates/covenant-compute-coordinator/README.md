# covenant-compute-coordinator

The marketplace half of the Covenant compute network: an axum service
that registers operator nodes, matches signed buyer jobs against
capability, price and reputation, holds the money in escrow while the
work runs, verifies the operator's signed receipt, and pays out. The
network-level map lives at
[docs/compute-network.md](../../../docs/compute-network.md).

```sh
cargo run -p covenant-compute-coordinator
# defaults: 127.0.0.1:8720, state under ~/.covenant-compute-coordinator
```

Configuration is env-only. The state directory holds a journal (escrow
holds, in-flight jobs, buyer deposits — replayed on boot, compacted in
the background) and the coordinator's own hash-chained audit log; lose
it and you lose the books, so a deployment mounts a persistent volume.
`deploy/Dockerfile.compute-coordinator` at the repository root packages
the binary with a `/data` volume and a `/health` check, and
`deploy/compute-render.example.yaml` is a ready blueprint.

## How a job moves

1. A buyer POSTs a `SignedJobEnvelope` to `/federation/jobs`; the
   envelope's price is held in escrow (402 if prefunding is enforced and
   the buyer's verified deposits don't cover it).
2. The matcher picks a live registered operator that fits kind,
   capability, price ask and the reputation floor; the offer rides the
   operator's long-poll. An offer nobody accepts within
   `COVENANT_COMPUTE_REOFFER_SECS` (default 60, 0 disables) is
   re-matched to whoever fits now — routing around an assignee that is
   provably sitting on the offer, redelivering after a restart lost
   the in-memory queues — and the move is audited, not faulted; a late
   decision from the previous assignee is refused. An operator that
   declares itself `Offline` (nodes do, the moment their model server
   dies or a drain begins) gets the same re-match for its still-queued
   offers as part of acking the beat — the window exists for silence,
   not honesty. Money never moves with the routing: if nobody can take
   the job, the deadline refund remains the backstop. The buyer holds
   the third exit: a signed POST to `/federation/jobs/:id/cancel`
   withdraws a job while it is still unaccepted — full refund now, no
   reputation fault on anyone, an honest retry echoed the same answer.
   Once an operator accepts, a cancel refuses: committed work settles
   by its result or its deadline, never a clawback.
3. The node runs the job and submits a `SignedWorkReceipt`. While a
   job that signed the `stream` flag runs, the assigned operator's
   session may push seq-ordered output chunks to
   `/federation/jobs/:id/stream` and the buyer drains them by signed
   cursor reads — an in-memory preview, never journaled, bounded per
   job and evicted after idling; the verified receipt output remains
   the artifact. The coordinator verifies the receipt against the
   assigned operator's key: verified `Ok` releases the hold and pushes
   the payout (a retry sweep re-pushes anything a crash interrupted);
   anything else refunds the buyer. The response body names the
   settlement — `released` or `refunded` — so the node's earnings
   books follow the verdict, never a bare 200. The transfer carries an SPL memo
   derived from the receipt itself, so the on-chain transaction names
   the work it paid for and anyone holding the receipt can verify the
   linkage.
4. Receipts, refunds, disputes, fees and payouts are all journaled and
   audit-chained; buyers and operators read their own history through
   signed reads. Every unpaid conclusion pins a `refund_reason` on the
   job record, and the receipt poll and both history feeds serve it —
   so a refunded row says which of `deadline_expired`,
   `buyer_cancelled`, `operator_rejected`, `execution_failed` or
   `admission_failed` sent the money back, and an operator can tell a
   fault its standing carries from a buyer who merely walked away.

## Keeping operators honest

- **Canary probes** — known-answer jobs bought with rotated fresh
  identities on a timer (`COVENANT_COMPUTE_CANARY_INTERVAL_SECS`);
  a wrong answer is a durable reputation fault.
- **Redundancy sampling** — released jobs re-bought from other operators
  and hash-compared by strict majority
  (`COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS`). Batch always; add
  deterministic inference (temperature 0 + a seed) with
  `COVENANT_COMPUTE_REDUNDANCY_INFERENCE=1`, which asserts the operator
  pool returns byte-identical output for such a request.
- **Reputation floor** — `COVENANT_COMPUTE_MIN_OPERATOR_SCORE_BPS`
  keeps proven-bad operators from winning organic work; probes still
  reach them, so passing probes is the road back. Standing is public:
  `/federation/operators/:operator/reputation` returns the counts
  behind the score, both floors, directory freshness and the matcher's
  verdict in one read (the node binary's `status` command renders it).
- **Disputes** — buyers file signed disputes within
  `COVENANT_COMPUTE_DISPUTE_WINDOW_SECS` (default 24h); the dispute
  pins to the job record with its evidence and faults reputation. The
  operator's signed books read carries the complaint back, so the
  accused party can see which job and why (the node binary's `status`
  lists disputed jobs verbatim).

## Money posture

Buyer revenue is the only money. A bootstrap subsidy must be explicitly
enabled (`COVENANT_COMPUTE_FUNDING_SOURCE=bootstrap` with
`COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS` and
`COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC`) and is refused entirely
when no policy is set — the kill-switch's off position. A running
subsidy can be killed without a deploy: `POST /federation/subsidy/close`
(admin bearer) refuses every bootstrap hold from that moment on. The
close is journaled before it latches and outranks the environment on
every later boot, so a restart that still carries the subsidy vars
comes up shut instead of silently re-armed; it is one-way on purpose —
an admin token can stop subsidy spend, never start it, and re-opening
means a fresh data directory. Organic money and in-flight bootstrap
holds are untouched (the latch stops new spend, it claws nothing back);
`/federation/subsidy` and the `compute_subsidy_closed` gauge say
which state you're in. The marketplace
fee (`COVENANT_COMPUTE_FEE_BPS`, default 0) is disclosed to every
registering operator and withheld at payout; referral partners earn
their share out of that fee, never out of operator pay. Inbound deposits
are verified against a Solana RPC rail (`COVENANT_COMPUTE_RAIL_RPC_URL`,
`_RAIL_DEPOSIT_OWNER`, `_RAIL_MINT`); payouts go through the bundled
sidecar signer when `COVENANT_COMPUTE_PAYOUT_BACKEND=sidecar`.
Registration is the first money gate: an operator whose payout address
doesn't decode to a real key gets a 400 naming the defect, never a seat
in the match book — admitted, it would win work whose payout push can
never land, and the retry sweep can't fix a typo.

A buyer's unspent balance stays theirs to withdraw: a signed request
to `/federation/buyers/withdraw` is arbitrated against holds under the
same escrow lock, so a withdrawal and a spend can't double-draw a
deposit. The debit journals first and the transfer (memo-tagged
`compute-withdrawal:v1:<buyer>:<id>`) rides the payout backend; a push
that never reached the chain is re-pushed by the retry sweep until it
lands, while one whose outcome can't be confirmed is held for
reconciliation rather than sent again, so a crash can't pay the same
withdrawal twice.

Operators can put stake behind their answers. A bond is an on-chain
transfer to the same receiving account deposits use, memo-tagged
`compute-bond:v1:<operator>` and claimed at
`/federation/operators/bond` — the rail, not the claimant, says whose
stake it is. `COVENANT_COMPUTE_MIN_BOND_MICRO_USDC` (default 0 = off)
floors matching on committed stake the way the score floor gates on
reputation. For a GPU lease priced by the hour,
`COVENANT_COMPUTE_MIN_BOND_LEASE_HOURS` (default 0 = flat) scales that
floor with the advertised rate — an operator must bond that many hours
of its own rate, so a premium card posts more stake than a cheap one and
advertising supply you cannot back is priced out. Only coordinator-proven
faults slash — a canary probe judged
wrong or a redundancy strict-majority minority, for the faulted job's
price — never a buyer dispute. Exit is a signed request to
`/federation/operators/unbond` that matures over
`COVENANT_COMPUTE_UNBOND_SECS` (default 24h) and stays slashable the
whole window: the refund (memo `compute-bond-refund:v1:<operator>:<id>`)
pays whatever survived, pushed by the same retry sweep. A pending unbond
already counts against the matching floor, so an operator heading for
the exit stops winning work before its money moves. The stake picture is
a signed read at `/federation/operators/:operator/bond`; posting
instructions are public at `/federation/bond-info`.

## Public settlement proofs

Anyone can check that the network paid for the work it billed, without
trusting the coordinator. Each settled job's operator-signed receipt and
the on-chain payout that honored it are published together as a proof:
verify the receipt's signature, recompute the memo it commits to, then
fetch the cited transaction and confirm it paid that amount, in that
mint, to that operator. The receipt carries hashes and the operator's own
metered figures. It never carries the buyer's identity or the job's input
or output, so a proof settles the payment question without exposing who
bought what.

The feed is off by default; every receipt read stays buyer-gated until a
deployment sets `COVENANT_COMPUTE_PUBLIC_PROOF_FEED=1` and settles on a
chain. `GET /proof/receipts` lists settled jobs newest first (page with
`?before_ms=` and `?limit=`, capped at 100); `GET /proof/receipts/:job_id`
returns one. The `covenant-compute-protocol` crate carries the whole
verifier: `SettlementProof::verify_offline` for the signature and the
release arithmetic, and `verify` against the fetched transaction for the
payment itself, so a third party reproduces every check with the same
code the coordinator runs.

`GET /proof/batch` folds every settled job into one Merkle root over the
proofs, oldest conclusion first — a binding commitment to exactly that
set. `GET /proof/batch/:job_id` returns a `BatchInclusionProof` — the
settlement plus the audit path from its leaf to the root — so any one
settlement's membership is checked with a handful of hashes instead of
the whole feed. A reader that keeps a root and a proof can later show the
coordinator committed to that settlement, even if it drops from a later
feed. `BatchInclusionProof::verify_offline` walks the path and re-runs
the settlement's own checks; the tree promotes an odd node instead of
duplicating it, so a root binds one set of leaves. Proving the feed only
ever grew between two roots is a consistency proof, not yet served.

## Client-sealed vault

A buyer often needs a secret on the far side of a job — an SSH key for a
leased box, a model token, an env file — without handing it to whoever
runs the network. The vault holds that secret as ciphertext the buyer
sealed under a key the coordinator never receives. The store keeps bytes
it cannot read: nothing it retains recovers the plaintext, and a lost key
makes the secret unrecoverable by design.

The seal is XChaCha20-Poly1305 with a fresh random nonce per write, and
the key stays with the buyer. Each request is signed the way the
buyer-history reads are: the owner signs the method and path, and a store
also signs the exact ciphertext, so a captured request cannot be replayed
with a substituted secret or as a different operation. An owner reaches
only their own namespace, and the store bounds how many secrets one owner
keeps and how large each may be. Keypairs are free to mint, so a
deployment open to the internet also bounds distinct owners with
`COVENANT_COMPUTE_VAULT_MAX_OWNERS`, or fronts the vault with a proxy, the
way the operator registry is bounded.

Off unless a deployment sets `COVENANT_COMPUTE_VAULT=1`. Then
`POST /vault/:owner/secret/:label` stores a sealed secret, `GET` fetches
it, `DELETE` removes it, and `GET /vault/:owner/secrets` lists an owner's
labels — metadata only, never a ciphertext. The store is durable beside
the money journal and survives a restart. The buyer library's
`vault_store` / `vault_fetch` / `vault_list` / `vault_delete` drive the
whole loop; `examples/vault_roundtrip.rs` in the buyer crate runs it end
to end against a coordinator serving the vault.

## Operating it

| Surface | What it is |
| --- | --- |
| `/health` | Readiness: `ok` while the coordinator can persist fund state; `503` when its journal can no longer be written, so a health check restarts or reroutes the instance instead of sending money traffic to one that cannot record it |
| `/metrics` | Prometheus text: journal write health, operators, jobs by phase, disputes, fees, subsidy, and every money book summed (deposits, withdrawals, escrow by state, payouts, rev-shares, bonds) so conservation is scrapable — aggregate-only |
| `/federation/subsidy`, `/federation/fees` | the same transparency figures as JSON |
| `/federation/capacity` | the live (kind, model) supply directory with ask ranges and the floors in force — aggregate-only, no operator identities; buyers read it to price a job, the node's `status` renders it as the market to compete in |
| `/proof/receipts`, `/proof/receipts/:job_id` | Public settlement proofs, off unless `COVENANT_COMPUTE_PUBLIC_PROOF_FEED=1`: each settled job's signed receipt and the on-chain payout that honored it, so anyone can verify the network paid for the work without trusting the coordinator. Hashes and the operator's own figures only, never the buyer or the job's contents |
| `/proof/batch`, `/proof/batch/:job_id` | The same feed, committed: one Merkle root binding every settled job, plus a per-job membership proof against it, so a reader confirms one settlement without downloading the rest and can later show the coordinator committed to it. Same opt-in and same aggregate-only posture |
| `/vault/:owner/secret/:label`, `/vault/:owner/secrets` | Client-sealed secret store, off unless `COVENANT_COMPUTE_VAULT=1`: a buyer stores ciphertext sealed under a key the coordinator never receives, and reads it back over a signed request. The store holds bytes it cannot read; a listing returns labels and sizes, never a ciphertext |
| `Authorization: Bearer $COVENANT_COMPUTE_ADMIN_TOKEN` | admin surface (partner mark-paid, subsidy close); fail-closed until set |
| `COVENANT_COMPUTE_REQUIRE_PREFUNDED=1` | organic jobs need covering deposits — the open-internet posture |
| `COVENANT_COMPUTE_MAX_OPERATORS`, `COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER` | volumetric backstops when no reverse proxy fronts you |
| `COVENANT_COMPUTE_LONG_POLL_SECS` | how long an operator work poll holds before answering empty (default 30) — keep it under any fronting proxy's idle timeout |
| `COVENANT_COMPUTE_MIN_PROTOCOL` | wire-version floor for `/federation/*` (default 0 = admit every client, versionless included). Raised alongside a breaking wire change: older clients then get a 426 naming both versions instead of a shape error. Every reply carries `x-compute-protocol`; `/health` and `/metrics` never floor |

SIGTERM drains in-flight requests and exits 0; a hard kill is safe too —
the journal replays, and a boot reconciliation settles anything a kill
left half-written between the escrow ledger and the job book (an
orphaned hold refunds, a settled hold's verdict concludes its record).
A release whose receipt died with the crash is filled back in when the
operator re-submits it, and the payout it promised then pushes. Operator
registrations are deliberately not journaled: nodes re-register and
heartbeat on their own.

## Configuration reference

Every knob is an environment variable read once at boot. A knob with a
default is optional; a malformed value fails the boot with one line
naming the variable rather than starting on the wrong setting.

**Service**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_COORDINATOR_HOME` | `~/.covenant-compute-coordinator` | State directory: the identity key, the journal and the audit log. Mount it on a persistent volume. |
| `COVENANT_COMPUTE_COORDINATOR_BIND_ADDR` | `127.0.0.1` | Listen address. Set `0.0.0.0` to accept off-host traffic. |
| `COVENANT_COMPUTE_COORDINATOR_PORT` | `8720` | Listen port. |
| `COVENANT_COMPUTE_LONG_POLL_SECS` | `30` | How long an operator work poll holds before answering empty. Keep it under any fronting proxy's idle timeout. |
| `COVENANT_COMPUTE_VAULT` | `0` | Serve the client-sealed secret store at `/vault`. On, the coordinator keeps per-owner ciphertext it cannot read, durable under the state directory beside the journal. |
| `COVENANT_COMPUTE_VAULT_MAX_OWNERS` | unset | Ceiling on distinct vault owners; a volumetric backstop for an open deployment. Unset admits any number; a known owner is never turned away by it. |
| `COVENANT_COMPUTE_PUBLIC_PROOF_FEED` | `0` | Serve the public settlement proof feed at `/proof`. On, and settling on a chain, each settled job's signed receipt and its on-chain payout are published for anyone to verify. |

**Money, fees and funding source**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_FEE_BPS` | `0` | Marketplace fee in basis points, disclosed at registration and withheld at payout. |
| `COVENANT_COMPUTE_FUNDING_SOURCE` | `organic` | `organic` or `bootstrap`. Bootstrap requires both subsidy knobs below. |
| `COVENANT_COMPUTE_SUBSIDY_MAX_RATIO_BPS` | (unset) | Subsidy-to-organic ratio cap (≤ 10000). Required for bootstrap; opens the switch for bootstrap-tagged spend under organic. |
| `COVENANT_COMPUTE_SUBSIDY_FLOOR_MICRO_USDC` | (unset) | Subsidy budget floor. Set together with the ratio; with neither set, every bootstrap hold is refused. |
| `COVENANT_COMPUTE_PARTNERS` | (none) | Referral table, `code=address:share_bps` comma-separated. Shares are carved from the fee, so they earn nothing at a zero fee. |
| `COVENANT_COMPUTE_REQUIRE_PREFUNDED` | off | `1` requires organic jobs to be covered by verified deposits (the open-internet posture); otherwise holds are custodial promises. |

**Inbound deposit rail** (set all three, or none)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_RAIL_RPC_URL` | (unset) | Solana RPC the rail reads deposits and bond posts from. |
| `COVENANT_COMPUTE_RAIL_DEPOSIT_OWNER` | (unset) | The receiving account deposits and bonds are sent to. |
| `COVENANT_COMPUTE_RAIL_MINT` | (unset) | The SPL mint a creditable deposit must move. |

**Payout backend**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_PAYOUT_BACKEND` | `mock` | `mock` records intended transfers and moves nothing; `sidecar` signs real SPL transfers and requires every value below. |
| `COVENANT_COMPUTE_PAYOUT_SIGNER_BINARY` | (required for sidecar) | Path to the isolated signer binary. |
| `COVENANT_X402_RPC_URL` | (required for sidecar) | Solana RPC the payout transfers submit to. |
| `COVENANT_X402_FUNDING_KEYPAIR` | (required for sidecar) | Path to the funding wallet the signer draws from. |
| `COVENANT_COMPUTE_PAYOUT_MINT` | (required for sidecar) | The SPL mint payouts transfer. |
| `COVENANT_COMPUTE_PAYOUT_CAP_MICRO_USDC` | (required for sidecar) | Per-job payout ceiling. A job whose net would exceed it is refused before any hold. |
| `COVENANT_COMPUTE_OBLIGATION_CAP_MICRO_USDC` | `0` | Per-transfer ceiling on principal returns (withdrawals, unbond refunds). `0` leaves them bounded only by the books. |

**Trust, stake and disputes**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_MIN_OPERATOR_SCORE_BPS` | `0` | Reputation floor: operators below it win no organic work; probes still reach them. |
| `COVENANT_COMPUTE_MIN_BOND_MICRO_USDC` | `0` | Stake floor: operators without that much committed bond win no organic work. |
| `COVENANT_COMPUTE_MIN_BOND_LEASE_HOURS` | `0` | Scales the stake floor for a GPU-hour lease operator: it must bond this many hours of its own advertised rate, above the flat floor. `0` keeps one flat floor for all. |
| `COVENANT_COMPUTE_UNBOND_SECS` | `86400` | How long an unbond matures before its refund pushes; the stake stays slashable the whole window. |
| `COVENANT_COMPUTE_DISPUTE_WINDOW_SECS` | `86400` | How long after a job concludes its buyer can still file a signed dispute. `0` refuses every dispute. |

**Limits and protocol**

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_MAX_OPERATORS` | (unlimited) | Registry cap; registrations past it get 503. Leave unset behind a reverse proxy. |
| `COVENANT_COMPUTE_MAX_INFLIGHT_PER_BUYER` | (unlimited) | Per-buyer in-flight ceiling; submissions past it get 429. |
| `COVENANT_COMPUTE_MIN_PROTOCOL` | `0` | Wire-version floor for `/federation/*`; older clients get 426. `/health` and `/metrics` never floor. |
| `COVENANT_COMPUTE_ADMIN_TOKEN` | (unset) | Bearer token for the admin surface (partner mark-paid, subsidy close). Fail-closed until set. |
| `COVENANT_COMPUTE_PUBLIC_PROOF_FEED` | off | `1` serves the public settlement proof feed at `/proof/receipts` and its committed form at `/proof/batch`; off keeps every receipt read buyer-gated. Effective only when payouts settle on a chain, so a proof has a transaction to cite. |

**Background tasks** (all self-heal the books; `0` disables)

| Variable | Default | Meaning |
| --- | --- | --- |
| `COVENANT_COMPUTE_REOFFER_SECS` | `60` | How long a delivered offer may sit unaccepted before it re-matches to whoever fits now. |
| `COVENANT_COMPUTE_PAYOUT_RETRY_SECS` | `60` | Cadence of the sweep that re-pushes released-but-unpaid jobs until the transfer lands. |
| `COVENANT_COMPUTE_JOURNAL_COMPACT_SECS` | `3600` | Cadence of background journal compaction. |
| `COVENANT_COMPUTE_CANARY_INTERVAL_SECS` | `0` (off) | Cadence of known-answer canary probes. Probe spend is bootstrap-tagged, so it also needs a subsidy policy open. |
| `COVENANT_COMPUTE_CANARY_MAX_PRICE_MICRO_USDC` | `10000` | Ceiling on what a canary probe offers. |
| `COVENANT_COMPUTE_CANARY_DEADLINE_MS` | `120000` | Deadline a canary probe gives the operator. Minimum 1000: a probe that expires before it can be served faults every honest operator it touches, so keep it generous. |
| `COVENANT_COMPUTE_REDUNDANCY_INTERVAL_SECS` | `0` (off) | Cadence of redundancy re-buys, hash-compared by strict majority. Mirror spend is bootstrap-tagged too. |
| `COVENANT_COMPUTE_REDUNDANCY_MIRRORS` | `2` | How many other operators re-run a sampled job. Minimum 2 when sampling is enabled: a strict majority needs the source plus two mirrors, so fewer can never fault a divergent operator. |
| `COVENANT_COMPUTE_REDUNDANCY_MAX_PRICE_MICRO_USDC` | `10000` | Ceiling on what a mirror re-buy offers. |
| `COVENANT_COMPUTE_REDUNDANCY_INFERENCE` | off | `1` also samples deterministic (temperature-0, seeded) inference, asserting the pool returns byte-identical output. |
