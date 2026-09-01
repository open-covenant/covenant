# W009/W011 reference enforcement witness

`w009-w011-20260731-v3.json` is a signed run of a fixed, scripted Memo-only
scenario in Covenant's standalone reference harness. Within that boundary,
W009 requires a signed, scoped capability before signing and W011 prevents the
scripted proposal after input declared untrusted. It is not evidence that a
production daemon mediated an external wallet.

Default trust documents are versioned under `trust/<run_id>/`. The CLI validates
the bundle run id before resolving that directory. The verifier pins the
authority root in
`services/witness-verifier/trust-anchor.mjs`. That root signs a policy with the
exact role-manifest hash, and the role manifest binds the agent, approver,
enforcer, and separately keyed verifier. Actor keys are not trusted from the
witness bundle itself. The reference tooling loads these role keys in one
process under one operator. They are cryptographically separate keys, not
independent operators or third-party attestations.

The W009 evidence contains:

- a denied sign attempt with no approval;
- a signed one-use capability grant scoped to devnet, fee payer, Memo program,
  empty account list, derived data commitment, expiry, and nonce;
- a signed authorization and execution plan;
- a Memo payload committing to the proposal hash, signed grant digest, and
  signed authorization digest;
- a signed, unique grant-consumption reservation claim;
- when recorded, a finalized devnet transaction signed by the capability
  subject, plus a separately signed execution record and local reservation
  digest.

The fixed W011 scenario contains:

- finalized Solana input explicitly marked untrusted;
- exact proposed legacy-message bytes and transaction scope declared as derived
  from that input;
- signed, declared event-parent lineage between the input, proposal,
  refutation, denial, and outcome;
- a refutation signed by the separately keyed verifier;
- an enforcer-signed denial;
- an enforcer-signed outcome with no signed transaction, no signature, and
  `submitted: false`.

The signed W011 outcome is static artifact evidence. Callback behavior is not
observable from that artifact. The reference tests separately assert that W011
does not invoke the callback supplied to `enforceW011` and that W009 replay is
blocked before submit. This does not prove dynamic taint tracking, semantic
classification of arbitrary input, lineage completeness, or general W011
enforcement. A direct signer or another submit callback outside the harness can
bypass this boundary.

The recorded v3 run binds an enforcer-signed digest claim for the canonical
exclusive-create journal written by the harness before submission, marked
`canonical_exclusive_fsync_file.v1`. This is signed local-state evidence, not
independently observable global replay state. The recorder uses one fixed
`consumptions-v1` directory, keys files by consumption hash, rejects alternate
caller stores, and awaits exclusive create, write, file `fsync`, and directory
`fsync` before signing or submitting. It also checks wall-clock expiry before
reservation, signing, and submission. Canonical executors contend atomically in
that namespace. Running legacy and canonical executors concurrently is
unsupported because their two namespaces do not share one atomic migration
lock.

Before its first asynchronous boundary, `executeAuthorizedW009` takes canonical
immutable snapshots of the bundle and trust documents, copies the secret key,
and derives every authorized signing field. The exported helper still accepts a
caller-provided blockhash and submit callback; it does not independently bind
an arbitrary callback to devnet. `record-devnet-transaction.mjs` supplies that
boundary by checking the devnet genesis hash, obtaining the devnet blockhash,
and wiring submission. Direct use of the helper has the caller's trust boundary.

Verify signatures, declared event-parent order, scope, Memo commitments, the
signed reservation claim and local reservation digest, and any recorded Solana
wire transaction offline:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v3.json
```

Require a finalized W009 execution record, but still remain offline:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v3.json \
  --require-devnet-record
```

Live RPC confirmation is a separate check. It verifies devnet genesis hash and
requires exact wire bytes, slot, block time, successful status, and finalized
confirmation for both cited transactions:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v3.json \
  --rpc https://api.devnet.solana.com
```

The execution recorder persists canonical one-use state before submission and
refuses expired grants, replayed consumption hashes, or an existing execution:

```sh
node services/witness-verifier/record-devnet-transaction.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v3.json \
  --keypair "$HOME/.config/solana/id.json" \
  --enforcer-key "$HOME/.config/covenant/witness-enforcement-v2/enforcer.pem" \
  --rpc https://api.devnet.solana.com
```
