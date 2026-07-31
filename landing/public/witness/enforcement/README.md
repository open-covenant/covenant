# W009/W011 reference enforcement witness

`w009-w011-20260731-v2.json` is a signed run from Covenant's standalone
reference harness. It is not evidence that a production daemon mediated an
external wallet.

The verifier pins the authority root in
`services/witness-verifier/trust-anchor.mjs`. That root signs a policy with the
exact role-manifest hash, and the role manifest binds the agent, approver,
enforcer, and separately keyed verifier. Actor keys are not trusted from the
witness bundle itself.

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

The W011 evidence contains:

- finalized Solana input explicitly marked untrusted;
- exact proposed legacy-message bytes and transaction scope derived from that
  input;
- a refutation signed by the separately keyed verifier;
- an enforcer-signed denial;
- an enforcer-signed outcome with no signed transaction, no signature, and
  `submitted: false`.

The signed W011 outcome is static artifact evidence. Callback behavior is not
observable from that artifact. The reference tests separately assert that W011
never invokes the submit callback and that W009 replay is blocked before
submit.

The recorded v2 run binds an enforcer-signed digest claim for the
exclusive-create journal recorded by the harness before submission, marked
`legacy_exclusive_file.v0`. This is signed local-state evidence, not
independently observable global replay state. The hardened recorder uses one
fixed `consumptions-v1` directory, keys files by consumption hash, rejects
alternate caller stores, and awaits exclusive create, write, file `fsync`, and
directory `fsync` before signing or submitting. It also checks wall-clock
expiry before reservation, signing, and submission.

Verify signatures, causal order, scope, Memo commitments, the signed
reservation claim and local reservation digest, and any recorded Solana wire
transaction offline:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v2.json
```

Require a finalized W009 execution record, but still remain offline:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v2.json \
  --require-devnet-record
```

Live RPC confirmation is a separate check. It verifies devnet genesis hash and
requires exact wire bytes, slot, block time, successful status, and finalized
confirmation for both cited transactions:

```sh
node services/witness-verifier/verify-enforcement.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v2.json \
  --rpc https://api.devnet.solana.com
```

The execution recorder persists canonical one-use state before submission and
refuses expired grants, replayed consumption hashes, or an existing execution:

```sh
node services/witness-verifier/record-devnet-transaction.mjs \
  --bundle landing/public/witness/enforcement/w009-w011-20260731-v2.json \
  --keypair "$HOME/.config/solana/id.json" \
  --enforcer-key "$HOME/.config/covenant/witness-enforcement-v2/enforcer.pem" \
  --rpc https://api.devnet.solana.com
```
