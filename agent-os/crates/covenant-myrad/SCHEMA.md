# `covenant.myrad.signal.v1` — proposed v1 schema

A provenance receipt for a Myrad behavior signal. It binds the artifact a buyer
receives to the set of Reclaim-verified contributions behind it, records what
the checks found, and verifies offline against a pinned key.

This is a proposal, written against the signal export and profiles table Myrad
sent. Everything in it is implemented and tested in `covenant-myrad`; the
naming, the thresholds, and the split of who computes what are all open.

## Why the receipt sits where it does

Reclaim proves the source: the activity is genuinely the contributor's, proven
on their device, raw data never leaving it. That half is solid and Covenant does
not touch it.

The step after it is the one a buyer cannot check. Verified contributions go
into a pipeline and an aggregate comes out, and the buyer is asked to take on
faith that it came from N distinct consenting people, each backed by a valid
proof, none counted twice, inside the window it claims. Nothing in the current
export lets them confirm any of that. The receipt covers exactly that step, and
claims nothing about the step before it.

## The receipt

```jsonc
{
  "schema": "covenant.myrad.signal.v1",

  "signal": {
    "provider": "netflix",
    "dataset_id": "myrad_netflix_v1",
    "record_type": "streaming_behavior_intelligence",
    "schema_standard": "myrad_streaming_intelligence_v2",
    "schema_version": "2.0",
    "cohort_id": "netflix_high_engagement_premium_drama",
    "segment_id": "netflix_high_engagement_drama_premium",
    // sha256 over the RFC 8785 canonical form of the delivered artifact.
    "delivered_sha256": "9b78962c697d944b…"
  },

  "evidence": {
    "contributors": 6,
    "merkle_root": "d0a09ebee683b238…",
    "leaf_schema": "covenant.myrad.contribution.v1",
    "commitment_scheme": "sha256(rfc8785(contribution))"
  },

  "freshness": {
    "generated_from": "2026-05-05T19:58:20.087Z",
    "generated_to": "2026-05-19T17:40:20.697Z",
    "window_days_claimed": 90,      // what the payloads declare
    "observed_span_months": 123     // what the activity actually spans
  },

  "integrity": {
    "status": "pass | warn | fail",
    "policy": { "min_k": 5, "require_reclaim_proof_ref": false },
    "findings": [
      { "check": "consent", "status": "pass", "detail": "no opted-out contributions", "affected": 0 }
      // one entry per check, always, including the passes
    ]
  },

  "issued_at": 1785882000
}
```

Signed form:

```jsonc
{
  "receipt": { /* the object above */ },
  "root_hash_hex": "…",              // sha256(rfc8785(receipt))
  "attestor_pubkey_b64": "…",
  "signature_b64": "…",              // ed25519 over the ASCII of root_hash_hex
  "anchor": "…"                      // Solana reference, added after signing, outside the signed bytes
}
```

Three properties worth stating because they drove the shape:

- **Warnings travel.** A receipt that dropped its soft findings would be worth
  less than no receipt. `findings` carries every check, and `status` is the
  worst of them.
- **The payload is opaque.** The receipt binds a digest of `sellable_data`, not
  its fields. Myrad can add, rename, or reorder anything inside it without a
  schema change on this side.
- **A failing cohort still gets a signed receipt.** Refusing to issue one would
  make the failure invisible. The receipt says `fail` and why; whether to sell
  is the seller's call.

## The leaf: `covenant.myrad.contribution.v1`

One per contributing record. This is what the Merkle root is built over, and it
is the only place the design needs anything from Myrad that the export does not
already carry.

```jsonc
{
  "schema": "covenant.myrad.contribution.v1",
  "provider": "netflix",
  "proof": { "id": "5a9f3035aa9cf93f653f4b15", "kind": "reclaim_proof" },
  "subject_commitment": "…",          // sha256(secret_salt || user_id), computed by Myrad
  "payload_sha256": "…",              // digest of that record's sellable_data
  "generated_at": "2026-05-19T17:40:20.697Z",
  "opt_out": false
}
```

Leaf value is `sha256(rfc8785(contribution))`. Leaves are sorted before folding,
so the root is a function of the set and not of query order. Leaf and interior
hashes are domain-separated (`0x00` / `0x01` prefix), and an odd node is
promoted rather than duplicated.

Two things this buys:

- **Membership without publication.** Myrad can show one contribution is under
  the root without revealing the rest of the set. A buyer auditing a dispute
  checks one leaf.
- **Distinctness without identity.** `subject_commitment` is a salted pseudonym.
  The same subject appearing twice in a cohort collides and is caught before the
  root is signed. Covenant never sees `user_id`, and the salt never leaves
  Myrad. k-anonymity hides who a contributor is; this is the part that stops one
  contributor being counted as five.

## The checks

Run over the cohort before signing. `fail` blocks a sale, `warn` is sold with
the buyer told.

| check | fails when | notes |
|---|---|---|
| `consent` | any contribution is marked `opt_out` | |
| `verification_status` | any contribution is not `verified` | Myrad's own verdict, passed through |
| `subject_uniqueness` | a `subject_commitment` repeats | warns when the commitment is absent, rather than passing silently |
| `payload_distinctness` | never fails | warns on byte-identical payloads under two subjects |
| `proof_reference` | a proof reference is in no known shape | warns on `polled_…` / `callback_…`; configurable to fail |
| `cohort_min_k` | cohort is below `min_k` (default 5) | |
| `temporal_bounds` | activity is dated after the record's own `generated_at` | |
| `window_consistency` | never fails | warns when activity spans more than `data_window_days` |
| `pii_surface` | never fails | warns on quasi-identifiers in a payload declaring `pii_stripped` |
| `activity_present` | never fails | warns on verified records with no measured activity |

`min_k = 5` is a starting default, not a recommendation we are attached to. It
is recorded in every receipt so two receipts are only comparable when they were
issued under the same bar.

## What the sample export produced

Running the reference pipeline over the 12-record export, grouped by `cohort_id`
(`cargo run -p covenant-myrad --example verifiable_signal -- signals.json profiles.csv`):

- **7 cohorts of 1 or 2 contributors.** Every one fails `cohort_min_k` at the
  default. If the sample was drawn thin on purpose this is expected; if
  production cohorts land at this size, the aggregate is close enough to a person
  that the k threshold is the conversation worth having first.
- **7 of 12 contributions reference `polled_…` or `callback_…`** rather than a
  24-hex proof id, while all 12 are marked `verified` / `zk_verified`. These
  probably resolve to real proofs inside your pipeline. Nothing outside it can
  follow them there, which is what the `proof_reference` warning says.
- **One record carries activity through 2027-06 on a record generated
  2026-05-19**, 13 months ahead. That reads like a date-parsing fault on the
  Netflix export rather than anything intentional, and it inflates volume in any
  aggregate that record lands in.
- **`data_window_days` is 90 on every record**, including one spanning 123
  months. The field looks like a constant rather than a measurement, so the
  receipt reports the observed span alongside it instead of trusting it.
- **`profile_name_initial` survives** (`"K"`, `"C"`, `"D"`, …) in payloads that
  declare `pii_stripped: true`. An initial is not a name, but with a cohort
  label and a multi-year activity curve it is a selector. Cheapest fix is
  dropping the field.
- **Two pairs look like the same person captured twice** (samples 05/06 and
  07/08: same initial, same totals, minutes or days apart, different `user_id`
  and different proof id). Not byte-identical, so exact-match dedupe misses them.
  If that pattern is real, `subject_commitment` needs to be minted from something
  stable per human, most likely the Reclaim-verified account, rather than per
  Myrad user row. That is the single most important open question here.

None of this needed a Reclaim proof or a raw record to find, which is the
argument for the layer.

## What we need from you

1. **A Reclaim proof sample.** The signal export and profiles table arrived; the
   proof sample did not. Until it does, the receipt says a contribution
   references a proof as attested by your pipeline, not that Covenant verified
   it. With a sample we wire real verification in and the wording gets stronger.
2. **How `subject_commitment` should be minted.** Which identifier is stable per
   human on your side, and confirmation that the salt stays with you.
3. **What actually gets sold.** The export is per-subject records. If buyers
   receive a cohort aggregate, we bind that artifact; if they receive the
   records, we bind those. The receipt handles both, but the shape of
   `delivered_sha256` should match your real product.
4. **Whether `min_k` is ours to propose.** If you already enforce a minimum
   cohort size before emission, the check should read your number, not ours.

## Verifying a receipt

No Covenant code required.

1. Recompute `sha256(rfc8785(delivered))`; it must equal
   `signal.delivered_sha256`.
2. Recompute `sha256(rfc8785(receipt))`; it must equal `root_hash_hex`.
3. Check `signature_b64` over the ASCII of `root_hash_hex` against a pinned
   attestor key.
4. Read `integrity.status`.

The attestor key must be pinned out of band. A receipt that carries its own key
is self-consistent by construction, which is why `myrad.verify_signal` takes an
`expectedAttestorPubkeyB64` and enforces it.

## Running it

```sh
cargo test -p covenant-myrad
cargo run -p covenant-myrad --example verifiable_signal
cargo run -p covenant-myrad --example verifiable_signal -- signals.json profiles.csv
cargo run -p covenant-myrad --example emit_bundle -- bundle.json
```

The paid endpoint is `GET /x402/myrad/signal/:cohort` on the Covenant x402
seller (Base, USDC), serving the signal and its receipt together. It does not
issue: the attestor key lives with the issuer, and the endpoint serves what it
was handed, so a buyer verifying against a pinned key does not have to trust the
delivery path.

Your export is contributor data and is not in this repository. The examples take
it as a file argument and fall back to a synthetic cohort.
