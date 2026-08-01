# Production Audit: Solana Agent Trust and Payment Enforcement

Date: 31 July 2026

## Executive Summary

Covenant has useful evidence, policy, audit, and settlement components, but the
public proposal collapsed them into a stronger product than the repository
implements. Registration is not real-world identity. A publisher signature is
not claim validation. Transfer history is not reputation. The existing spend
endpoint is not signer authorization, and the current witness does not prove
that a production wallet or daemon enforces W009 or W011.

The correct product boundary is a local payment-enforcement pipeline backed by
portable evidence. Public evidence readers should return typed observations.
Policy should produce an exact, expiring decision over a canonical payment
intent. An isolated signer must independently decode the final transaction,
match it to that intent, atomically consume a one-use authorization, and only
then sign. Settlement and separately keyed witnesses can make that execution
auditable. They cannot replace the enforcement boundary.

## Verified Current State

- The Solana x402 seller was corrected in draft PR #130 and the same bounded
  copy was observed on the live service. Discovery, OpenAPI, and the encoded Payment-Required route description
  now call the service bounded evidence and explicitly deny identity,
  reputation, claim-truth, W009/W011, and settlement-finality inferences.
- The public landing deployment still serves the previous x402 documentation
  until the corresponding source correction is merged and deployed. Repository
  state alone does not complete the public-copy cleanup.
- The standalone devnet witness is published separately in draft PR #129. Its
  v3 artifact hash is
  `ae7f6e906253e6d8385cd30e6c227e5729fa073f2541b0c38d0c914fe911c89f`;
  this is evidence for that harness only.

## Critical Issues (P0 - Block Production Claims)

- [ ] Complete removal of identity, reputation, delivery, and generalized
  verification overclaims from repository source. The deployed x402 seller is
  corrected; repository-wide source review remains a merge gate.
- [ ] Deploy the corrected landing-site copy. The current public docs deployment
  still contains the superseded identity-passport, settlement-reputation, TDX,
  and never-charged language.
- [ ] Resolve the seller-base dependency advisory before production deployment.
  `npm audit --omit=dev` reports two affected production dependency nodes, one
  high and one moderate, both originating from `axios@1.16.0` in the
  `@coinbase/x402` -> `@coinbase/cdp-sdk` chain. As of 31 July 2026, the latest
  CDP SDK still pins that Axios version and `npm audit fix --dry-run` makes no
  change. Do not force an unvalidated override beneath the exact SDK pin:
  upgrade when the upstream graph permits it, or independently qualify and test
  a replacement before enabling facilitator-backed production traffic.
- [x] Stop server-side fetching of an onchain-controlled registration URI. The
  previous HTTPS-only fetch still allowed SSRF through private resolution,
  redirects, or rebinding.
- [x] Relabel configured DAS/RPC output as provider-backed structural
  observations rather than authenticated Core proofs.
- [x] Mark the existing spend-authorization endpoint as advisory. The caller
  supplies the proposed cap and budget units, the result is not transaction
  bound, and no signer consumes it.
- [ ] Define and enforce a `wallet.spend.authorize` scope. The permissions
  grammar has no wallet namespace today, so its capability does not bind
  provider, network, asset, recipient, cap, nonce, or final transaction; with
  peer self-grants it is not an approval boundary at all.
- [x] Park every daemon-reachable outbound x402 payment path, including direct
  `PayX402`, Hyre paid execution, Circuit paid tools, and AceData's keyless
  fallback. The reusable clients and explicit manual tooling remain available
  for development, but the production daemon does not move funds through them.
- [x] Park the peer-reachable SAP publish, audit-root, and attestation handlers.
  They now fail before bridge, network, or signer access.
- [x] Park daemon-owned Metaplex and SNS funded tools. They are filtered from
  discovery and refused before capability, sidecar, RPC, or key access; the
  lower-level clients remain explicit development surfaces.
- [ ] Treat AceData and Hermes API calls as externally billed operations. The
  daemon has no durable provider-credit reservation or hard monetary budget;
  AceData currently records zero local cost, and a Hermes budget may be absent
  or disabled. Model or tool scope is not spend policy.
- [x] Park the standalone MIP draft. Metaplex already has unfinished validation
  and reputation programs in its official `mpl-agent` repository; creating a
  parallel Covenant standard is premature.
- [x] Park the legacy escrow release reporter. Caller-supplied payout fields
  are no longer converted into a zero-credit settlement receipt or an
  `EscrowReleased` audit row; the compatibility request now fails closed
  without writing state.
- [ ] Do not describe W009 or W011 as production enforcement until the production
  signer and action executor require the checks. The signed devnet witness is a
  standalone reference harness only.
- [ ] Replace peer self-grants with operator-authorized delegation. Every
  authenticated peer can currently ask the daemon to sign an arbitrary action
  for itself with empty scope, so a capability is not evidence of operator
  approval.
- [ ] Stop passing the daemon environment and host `HOME` into trusted-local
  agent subprocesses. The current allowlist still includes credential-bearing
  `COVENANT_*` values, signer paths, keyed RPC URLs, and the daemon data
  directory. Use an explicit non-secret allowlist, an isolated home/user
  boundary, and a broker for every signer or billing credential.
- [x] Clear ambient environment inheritance for external stdio MCP servers.
  They now receive only `PATH` and explicitly configured per-server variables;
  same-user filesystem access and deliberately configured secrets remain
  outside that fix.
- [x] Park funded SAP and Metaplex automatic anchor drivers. Their legacy flags
  now produce a warning and the production binary starts no driver.
- [x] Disable redirects for signed payment retries so a custom payment header
  cannot be forwarded to another origin.
- [x] Reject zero-credit outbound payment requests and record the selected live
  requirement rather than only the caller's cap. This path remains parked.
- [x] Remove automatic retry after a paid-resource error or timeout. The
  automatic retry remains removed while daemon-owned outbound payment is
  parked; any future re-enable requires onchain settlement reconciliation
  because resource delivery and payment finality are separate outcomes.

## High Priority (P1 - Required Before Wallet Launch)

- [x] Define a strict, canonical `PaymentIntentV1`, trusted local policy, stable
  denial reasons, and an advisory receipt whose wire contract cannot claim
  signer enforcement.
- [x] Reject JSON numeric fields above JavaScript's exact integer range during
  deserialization, hashing, evaluation, and receipt serialization so Rust and
  the published schemas accept the same documents.
- [x] Distinguish free first-response success from a 402 payment-header retry in
  the legacy outbound client, and record the exact selected live requirement
  rather than the caller's per-call cap. This still does not prove settlement.
- [x] Bind caller-reported spend settlement to one approved payer and exact
  stored facts, namespace receipt IDs by payer, and retain idempotency claims
  through compaction/restart. This is process-local accounting, not transaction
  or chain verification.
- [ ] Implement x402 v2 challenge parsing and recompute the selected requirement
  hash from received bytes. The current reusable outbound Solana signer emits a
  legacy v1 envelope.
- [ ] Build or decode the final Solana transaction inside the signer boundary.
  Require exact matching of network, mint, token program, funder, fee payer,
  source account, recipient, destination account, amount, Memo, allowed
  programs, writable accounts, compute budget, and request binding.
- [ ] Bind trusted destination, canonical endpoint origin/path, exact scheme,
  fee payer, and redirect policy before invoking the legacy signer. Today a
  matched challenge can still choose `payTo` and related fields within the
  amount/network/asset cap; the new preflight policy is not wired to that path.
- [ ] Require an approver-signed, scoped, expiring, one-use capability whose
  subject is the signing key and whose digest commits to the exact intent and
  final transaction.
- [ ] Atomically reserve capability consumption before signing. The current
  reference journal is local single-host state; production needs crash-safe
  recovery and an explicit multi-process or distributed ownership model.
- [ ] Keep the key in a genuinely isolated signer boundary with authenticated IPC
  and an allowlisted request contract. A subprocess alone is not isolation.
- [x] Clear the SAP worker environment and make key access command-specific.
  The payer key is available only to payer-authorized commands, the verifier
  key only to verifier attestation, and read/status/stats commands receive no
  keys. Same-user filesystem access remains outside this process-level fix.
- [ ] Propagate untrusted-input provenance as typed runtime data from ingestion to
  the proposed action. A supplied event label or post-hoc log scan does not
  establish causality or completeness.
- [ ] Make the W011 verifier consume the exact proposed transaction and typed
  lineage before authorization. A separately keyed verifier under the same
  operator is useful fault separation, not an independent party.
- [ ] After submission, bind the authorization, transaction signature, finality,
  resource response, and receipt into one reconciliation record. Never infer
  delivery or quality from settlement alone.
- [ ] Replace or migrate the legacy `ExternalPaymentSettled` event name and
  reconcile signed-but-ambiguous retries. The current row means a matching 402
  was answered and the paid retry returned success; it is not chain-finality
  evidence, and failed retries may still have moved funds.

## Medium Priority (P2 - Product Hardening)

- [ ] Replace DAS-only authority matching with direct Core account decoding or a
  cryptographic account proof. Preserve provider, slot, commitment, and coverage
  metadata on every observation.
- [ ] Separate evidence schemas by meaning: registration, publisher-key-signed
  statement, payment observation, buyer acceptance, delivery assertion, and
  third-party evaluation. Do not aggregate them into a universal trust score.
- [ ] Version public endpoint semantics. Keep the legacy PayAI heuristic clearly
  named until it can be retired without breaking clients.
- [x] Pin publisher-signature verification to an externally supplied expected
  key and reject attacker self-signatures and malformed key material in both
  x402 seller implementations.
- [x] Reject ephemeral Base seller attestors on Base mainnet or in production;
  the explicit development override is limited to non-production test networks.
- [x] Make the legacy witness verifier sign the verdict and refutations as well
  as the root. The page remains yellow because its key is self-published beside
  the statement, and its Solana cards inspect manifests rather than RPC.
- [ ] Add replay, crash-recovery, concurrent signer, RPC equivocation, stale
  blockhash, fee-payer substitution, Token-2022, address lookup table, malicious
  Memo, redirect, and settlement-timeout integration tests.
- [ ] Publish machine-readable coverage and limitation fields with every public
  witness so consumers do not infer more than the verifier checked.

## Low Priority (P3 - Technical Debt)

- [ ] Split the standalone witness verifier into smaller schema, trust, Solana
  wire, W009, W011, storage, and RPC modules before it becomes production code.
- [ ] Add metrics for preflight reason codes, signer denials, authorization
  consumption, reconciliation lag, provider errors, and unresolved settlement
  outcomes without logging secrets or full sensitive payloads.
- [ ] Establish key rotation and revocation documents for authority, approver,
  enforcer, and verifier roles.
- [x] Require a commit-scoped verifier public key for every accepted v2 artifact
  and treat the global key file only as a latest-key compatibility pointer.
  Missing commit keys are red, and commit-scoped files are write-once unless an
  interrupted run resumes with byte-identical content. These keys remain
  self-published and therefore yellow, not external trust roots.

## Security Assessment

The immediate SSRF issues violated the design rule expressed by W011: an
untrusted URI reported by the chain/indexer reached a server-side network
fetch, and the passport page derived another server-side fetch target from
forwarded host headers. The fixes remove the registration-document fetch and
use only configured internal site URLs. This was a W011-class failure, not
evidence that the production runtime implemented general taint enforcement.
Reintroducing arbitrary URI fetching requires resolved-IP private-range rejection,
redirect revalidation or disabled redirects, port restrictions, response
limits, rebinding-resistant connection behavior, and tests against IPv4, IPv6,
and encoded-address bypasses.

The devnet W009/W011 artifact has a pinned authority root, root-signed role
manifest, distinct role keys, signed causal records, exact Solana wire bytes,
an approver-scoped one-use grant, a pre-sign durable local reservation, and live
RPC-confirmed transactions. That is credible evidence for its stated
standalone harness. It does not show that Covenant's production daemon mediated
an external wallet, that arbitrary runtime inputs retain taint, that omitted
events are detectable, or that replay is prevented across hosts.

The payment preflight contract closes its JSON shape, uses canonical
domain-separated hashes, binds exact policy values, denies malformed policy,
and serializes false-only enforcement flags. Its timestamp is supplied by the
evaluator and the receipt is unsigned, so deterministic replay checks only
self-consistency; it is not an authorization, independent attestation, or proof
of when a check occurred.

The retained escrow completion statement now signs
`covenant.escrow-completion.v1\n || proof_json`, and the bundle exposes the
domain for exact verification. The statement still mixes daemon-derived run
fields with caller-supplied escrow context and is not a release authorization.
Release reporting stays disabled until payout finality and the prior
authorization can be independently verified and committed atomically or with a
recoverable protocol.

## Performance Assessment

The advisory preflight is deterministic and bounded by small local policy
lists. Production code should cap fee-payer and route list sizes or index them
at configuration load. DAS pagination is currently bounded, but a five-page
scan by authority is too expensive and incomplete as a long-term lookup model.
Use subject-indexed records or a program-derived address for deterministic
lookup.

## Observability Assessment

Current evidence can be inspected after the fact, but there is no single
production correlation object spanning intent, approval, signer decision,
transaction, settlement, resource response, and retry state. Until that exists,
operators cannot reliably distinguish denied, signed-but-unsubmitted,
submitted-pending, settled-with-resource-error, and fully reconciled calls.

## Recommended Architecture

1. Parse an untrusted x402 challenge into a closed canonical intent.
2. Bind the exact HTTP request and selected payment requirement.
3. Evaluate trusted local policy and, when required, obtain a signed scoped
   approval.
4. Send the intent, approval, and candidate transaction to an isolated signer.
5. Decode the transaction inside that boundary and require an exact match.
6. Atomically consume the authorization before signing.
7. Sign and submit once, preserving the exact wire bytes and transaction ID.
8. Reconcile finality and resource delivery as separate outcomes.
9. Publish typed evidence and a separately keyed witness over the complete
   correlation record.

## Test Coverage Gaps

The new advisory contract has schema, canonical-hash, false-only boundary,
denial-order, and mutation tests. The standalone witness has offline signature,
wire-transaction, replay, callback, and live devnet checks. Missing production
tests are the boundary itself: real daemon-to-signer authenticated IPC, final
transaction decoding, concurrent one-use consumption, restart recovery,
malicious challenge handling, and end-to-end x402 v2 settlement reconciliation.

## Action Plan

1. Merge the public-truth, parked-payment, and advisory-preflight changes without
   calling them enforcement.
2. Publish the devnet witness in a separate draft PR with its standalone-harness
   boundary in the title, README, verifier output, and PR body.
3. Implement the signer-bound intent consumer as the next isolated milestone.
4. Contribute to Metaplex's existing agent-validation work only after agreeing
   on one concrete missing interface; do not submit the parked MIP as written.
5. Return to ecosystem catalog listings only after the relevant artifact is
   merged and the listing states its exact devnet or production boundary.
