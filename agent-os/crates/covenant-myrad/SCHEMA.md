# `covenant.myrad.signal.v1`: proposed v1 schema

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

Emitted, not illustrative: this is `cargo run -p covenant-myrad --example emit_bundle`
over the six-contribution reference cohort, trimmed to one finding for length.

```jsonc
{
  "schema": "covenant.myrad.signal.v1",

  "signal": {
    "provider": "netflix",
    "dataset_id": "myrad_netflix_v1",
    "record_type": "streaming_behavior_intelligence",
    "schema_standard": "myrad_streaming_intelligence_v2",
    "schema_version": "2.0",
    "cohort_id": "netflix_high_engagement_drama",
    "segment_id": "netflix_high_engagement_drama",
    // sha256 over the RFC 8785 canonical form of the delivered artifact.
    "delivered_sha256": "9b78962c697d944b6ec507390f3db672656c35eaa52ecb81639321cb358f4fca"
  },

  "evidence": {
    "contributors": 6,
    "merkle_root": "d0a09ebee683b238999630c8efc0bfda41a188c95baa90659204b30e93610be4",
    "leaf_schema": "covenant.myrad.contribution.v1",
    "commitment_scheme": "sha256(rfc8785(contribution))"
  },

  "freshness": {
    "generated_from": "2026-05-19T17:40:20.697Z",
    "generated_to": "2026-05-19T17:40:20.697Z",
    "window_days_claimed": 90,      // what the payloads declare
    "observed_span_months": 2       // what the activity actually spans
  },

  "integrity": {
    "status": "pass",               // pass | warn | fail, the worst of the findings
    "policy": { "min_k": 5, "require_reclaim_proof_ref": false },
    "findings": [
      { "check": "consent", "status": "pass", "detail": "no opted-out contributions", "affected": 0 }
      // one entry per check, always, including the passes
    ]
  },

  "issued_at": 1785880397
}
```

A descriptor field is emitted only when every contribution agrees on it, so a
mixed set produces a receipt that omits the field rather than one labeled with
whichever record sorted first. In your export `segment_id` is always equal to
`cohort_id`; if that holds generally, one of the two should come out of v1.

Signed form:

```jsonc
{
  "receipt": { /* the object above */ },
  "root_hash_hex": "018156deb7b3ab8a3ee9690e3aa20ef76dd4be7faa93953637b220a0503fc9b5",
  "attestor_pubkey_b64": "GX9rI+FshTLGq8g4+s1ep4m+DHaykgM0A5v6iz02jWE=",
  "signature_b64": "QPpkhVxqOKZ4X1MR4Mu3fjJN…",  // ed25519 over the ASCII of root_hash_hex
  "anchor": "…"  // reserved, absent until set; outside the signed bytes
}
```

`issued_at` is wall-clock, so two issuances of the same cohort differ in
`root_hash_hex`. A receipt identifies one issuance, not one cohort.

The key above is a demo key (`[42u8; 32]`), stable across runs and derivable by
anyone. Production issuance uses the Covenant attestor and the buyer pins its
public half out of band.

Three properties that drove the shape:

- **Warnings travel.** A receipt that dropped its soft findings would be worth
  less than no receipt. `findings` carries every check, and `status` is the
  worst of them.
- **The payload is opaque.** Two digests, neither of which commits to a field
  name: `signal.delivered_sha256` binds the artifact the buyer receives, and each
  contribution's `payload_sha256` (in its leaf) binds that record's payload.
  Myrad can add, rename, or reorder anything inside the payload without a schema
  change on this side.
- **Integers stay inside 2^53.** RFC 8785 canonicalizes numbers through the
  ECMAScript double rule, so two artifacts differing only above 2^53 would
  canonicalize identically and share a digest. Issuance refuses an artifact
  carrying one, and verification fails closed on the same input. A 64-bit id, a
  nanosecond timestamp, or a base-unit token amount belongs in the artifact as a
  string.

## The leaf: `covenant.myrad.contribution.v1`

One per contributing record. This is what the Merkle root is built over, and it
is the only place the design needs anything from Myrad that the export does not
already carry.

```jsonc
{
  "schema": "covenant.myrad.contribution.v1",
  "provider": "netflix",
  "proof": { "id": "5a9f3035aa9cf93f653f4b15", "kind": "reclaim_proof" },
  "subject_commitment": "…",          // sha256(secret_salt || "|" || user_id), computed by Myrad
  "payload_sha256": "…",              // digest of that record's sellable_data
  "generated_at": "2026-05-19T17:40:20.697Z",
  "opt_out": false
}
```

The commitment is `sha256(rfc8785(contribution))` rendered as **lowercase hex**,
and the Merkle leaf hashes that hex text, not the raw digest:

```
commitment = lowercase_hex(sha256(rfc8785(contribution)))
leaf       = sha256(0x00 || ascii(commitment))
node       = sha256(0x01 || left || right)     // over raw 32-byte digests
```

Leaves are sorted lexicographically by that hex before folding, so the root is a
function of the set and not of query order. The `0x00` / `0x01` tags keep an
interior node from being passed off as a leaf, which is what makes promoting an
odd node safe where duplicating it would not be. A repeated commitment is
rejected rather than folded in twice: one record copied five times would
otherwise produce a root over five leaves and a receipt claiming five
contributors.

Two things this buys:

- **Membership without publication.** Myrad can show one contribution is under
  the root without revealing the rest of the set. A buyer auditing a dispute
  checks one leaf against the `merkle_root` in the receipt they already hold:

  ```jsonc
  { "commitment_hex": "…", "path": [ { "sibling_hex": "…", "sibling_is_left": true } ] }
  ```

  Fold the leaf up the path and compare. This is issued on request rather than
  bundled, since a bundle carrying every path would publish the set it exists to
  keep private.
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
| `verification_status` | a contribution's status is outside `verified` / `zk_verified` | Myrad's own verdict, passed through; the observed value is named in the finding |
| `subject_uniqueness` | a `subject_commitment` repeats | warns when the commitment is absent, rather than passing silently |
| `proof_uniqueness` | one proof reference backs two contributions | the same evidence counted twice, whatever the pseudonyms say |
| `payload_distinctness` | never fails | warns on byte-identical payloads under two subjects |
| `proof_reference` | a proof reference is in no known shape | warns on `polled_…` / `callback_…`; configurable to fail |
| `cohort_min_k` | cohort is below `min_k` (default 5) | |
| `temporal_bounds` † | activity is dated after the record's own `generated_at` | |
| `window_consistency` † | never fails | warns when activity spans more than `data_window_days` |
| `pii_surface` † | never fails | warns on quasi-identifiers in a payload declaring `pii_stripped` |
| `activity_present` † | never fails | warns when a record reports `total_titles_watched: 0` |

† These four read the streaming payload shape (`viewing_summary`,
`viewing_behavior`, `user_profile`). Against a provider whose payload puts those
elsewhere they report `warn` with `not evaluated: N contribution(s) carry no …`,
never a pass. Wiring a new provider means telling the checks where its
equivalents live.

A failing cohort still gets a signed receipt, saying `fail` and why. Withholding
it would make the failure invisible; whether to sell on top of that is the
seller's call.

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
  24-hex proof id, while all 12 carry `verification_status: "verified"` and
  `metadata.verification.status: "zk_verified"`. These probably resolve to real
  proofs inside your pipeline. Nothing outside it can follow them there, which is
  what the `proof_reference` warning says. Confirming that `zk_verified` is a
  status the top-level field can also carry would be useful; the check accepts
  both today.
- **One record carries activity through 2027-06 on a record generated
  2026-05-19**, 13 months ahead. That reads like a date-parsing fault on the
  Netflix export rather than anything intentional, and it inflates volume in any
  aggregate that record lands in.
- **`data_window_days` is 90 on every record**, including one spanning 123
  months. The field looks like a constant rather than a measurement, so the
  receipt reports the observed span alongside it instead of trusting it.
- **7 of 12 records carry an empty `monthly_pattern`**, so
  `temporal_bounds` and `window_consistency` could not run on them and report
  `not evaluated` rather than a pass. Whether that is a thin sample or a real gap
  in the enrichment is worth knowing, because those two checks are how a buyer
  prices recency.
- **`profile_name_initial` survives** (`"K"`, `"C"`, `"D"`, …) in payloads that
  declare `pii_stripped: true`. An initial is not a name, but with a cohort
  label and a multi-year activity curve it is a selector. Cheapest fix is
  dropping the field.
- **2 pairs look like the same person captured twice** (samples 05/06 and
  07/08: same initial, same totals, minutes or days apart, different `user_id`
  and different proof id). Not byte-identical, so exact-match dedupe misses them.
  If that pattern is real, `subject_commitment` needs to be minted from something
  stable per human, most likely the Reclaim-verified account, rather than per
  Myrad user row. That is the single most important open question here.

None of this needed a Reclaim proof or a raw record to find.

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

## Open in v1, deliberately

Named rather than quietly decided, since each is a joint call:

- **Key rotation.** A receipt carries the attestor's public key but no key id, so
  a buyer pinning one key has nothing to follow when it changes. A key id plus a
  published validity range is the obvious answer; it is not written yet.
- **Unknown versions.** A verifier meeting `covenant.myrad.signal.v2` should
  refuse rather than guess, and this crate refuses anything that is not the exact
  v1 string. What a v2 is allowed to change is undefined.
- **Provider mapping.** The four checks marked † need to be told where a
  non-streaming payload keeps its equivalents. There is no configuration format
  for that yet, which is why they report `not evaluated` instead of guessing.

## What the endpoint delivers

The receipt travels with the artifact it covers, in one envelope:

```jsonc
{
  "resource": "covenant.myrad.signal.bundle.v1",
  "signal":   { /* the delivered artifact: this is what step 1 hashes */ },
  "receipt":  { /* the signed form above */ },
  "verify":   { "steps": [ /* the four steps below, restated inline */ ] }
}
```

`signal` is the artifact and `receipt.receipt.signal` is the descriptor of it;
they are different objects and only the first is hashed in step 1.

## Verifying a receipt

No Covenant code required. All four reproduce against both a synthetic and a
real bundle with `hashlib`, `json`, and an ed25519 library.

1. Recompute `sha256(rfc8785(bundle.signal))`; it must equal
   `receipt.receipt.signal.delivered_sha256`.
2. Recompute `sha256(rfc8785(receipt.receipt))`; it must equal
   `receipt.root_hash_hex`.
3. Check `receipt.signature_b64` over the ASCII of `receipt.root_hash_hex`
   against a pinned attestor key.
4. Read `receipt.receipt.integrity.status`.

Step 2 is load-bearing and cannot be skipped. The signature covers the digest,
not the receipt, so step 3 alone still passes over a receipt whose fields have
been edited. Steps 2 and 3 together are what bind them.

The attestor key must be pinned out of band. A receipt that carries its own key
is self-consistent by construction, which is why `myrad.verify_signal` takes an
`expectedAttestorPubkeyB64` and enforces it when supplied. Omit it and the result
says `attestor_pin_supplied: false` rather than implying an issuer check that did
not happen.

## Running it

From the `agent-os` directory of the repository:

```sh
cargo test -p covenant-myrad

# Every cohort in an export, with its checks. No arguments runs a synthetic cohort.
cargo run -p covenant-myrad --example verifiable_signal
cargo run -p covenant-myrad --example verifiable_signal -- signals.json profiles.csv

# One bundle, ready to serve: the largest cohort in the export, or the synthetic one.
cargo run -p covenant-myrad --example emit_bundle -- bundle.json
cargo run -p covenant-myrad --example emit_bundle -- bundle.json signals.json profiles.csv
```

The profiles CSV is optional and supplies what the payload does not: `user_id`,
from which the examples derive `subject_commitment`, and `opt_out`. Without it
`subject_uniqueness` reports `warn` rather than passing, because distinctness is
then unproven. The examples use a hardcoded demo salt to stand in for yours; in
production the commitment arrives already computed and Covenant never holds a
salt or a user id.

Serving it:

```sh
COVENANT_MYRAD_BUNDLE=bundle.json COVENANT_BASE_PAYTO=0x… npm start   # services/x402-seller-base
curl -i localhost:10000/x402/myrad/signal/netflix_high_engagement_drama   # 402 until paid
```

The paid endpoint is `GET /x402/myrad/signal/:cohort` on the Covenant x402
seller (Base, USDC), serving the signal and its receipt together. It does not
issue: the attestor key lives with the issuer, and the endpoint serves what it
was handed, so a buyer verifying against a pinned key does not have to trust the
delivery path. With no issued bundle configured it answers 503, since anything
below 400 would settle a payment for a body with no receipt in it.

Your export is contributor data and is not in this repository. The examples take
it as a file argument and fall back to a synthetic cohort.
