# x402 inference release evidence

Release candidate based on Compute deployment revision
`983172ddd04ac2de70136125e2088f22c6edf505`. This is a separate service; the
coordinator, Solana USDC economics, CVNT staking and existing fees are unchanged.
It uses the pinned official x402 V2 Solana implementation and a real Coinbase
AgentKit signing provider. Compatibility does not imply a commercial partnership.

## Local implementation

- Rust seller persists payment authorization, native signed job, verified result
  and settlement state before external actions. Original payment/job IDs survive
  retries and restarts. Finalized exact-transaction readback releases the result.
- The private verifier uses `@x402/svm` and `@x402/core` 2.28.0. Request-specific
  memo binding is signed by the standard client. Fee sponsorship is bounded.
- The native buyer enforces a durable spend budget and an approved operator set.
  The seller's lifetime reserve retains failed and uncertain operator costs.
- The AgentKit 0.10.4 client invokes a real registered action and wallet signature,
  checks merchant/network/asset/operator/request/output/receipt bindings, and keeps
  durable cumulative payment reservations. It is not an autonomous planning demo.
- The provider adapter has private loopback access, fixed model/input/output
  bounds, provider price ceilings and persistent reservations. Unknown requests
  cannot be automatically retried or assigned a fresh authorization.

The Node24 client has passed source and clean packed-package tests with actual
AgentKit signatures, the official verifier and a native Rust receipt fixture.
The release checks include 22 client tests, nine Rust gateway tests, four official
verifier tests, 45 provider tests and 52 focused OpenAI executor tests. The final
provider/executor checks cover the exact OpenRouter model mapping, contradictory
route refusal, signed model identity, and replay without another provider call.
Local fixtures and installed-package checks remain distinct from the live purchase
and restart evidence below.

## Production evidence

The new operator's stake was explicitly approved and submitted on Solana mainnet:

- Operator: `9fefASd52qfR2Q8P81yXtkHu9E4q9KvW58x2ixBAAvLR`
- Stake: 1,000,000 CVNT; 30 days plus the CLI's ten-minute clock tolerance.
- Position: `5eA1C663UaWNrPaSCNXciAcCyxZDn3fopouK8bDp7pMF`
- Lock expiry: 2026-11-17 00:01 UTC.
- [Confirmed stake transaction](https://solscan.io/tx/nNT9c5vT2NdGWNPR8nZdm8RNNbc1ezApkg89S6Mgb2N4sC8UfD1ow35XfbrbVM1L5JqSyca6WGEpqejpqcivSUj)

Provider qualification passed for the exact canonical model `claude-haiku-4.5`
on 2026-10-18. The response returned the requested marker, used 15 input and
8 output tokens, and completed in 1.194 seconds. Two earlier requests using a
dated model identifier failed strict response-model validation. Their full
0.600480 USDC combined reservations remain counted as immutable audited losses;
the original request identities can never be retried. The successful call counts
83 micro-USDC at the configured price ceilings. No customer payment was involved.

## Deployed service and immutable image

The separate service was promoted with a persistent volume and verified at source
revision [`e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`](https://github.com/open-covenant/covenant/commit/e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e):

- Gateway: `https://compute-inference.opencovenant.org`
- Image: `ghcr.io/open-covenant/covenant/compute-node-public@sha256:aef10b2661b20d35202ffdbc68ab3efb249c8e9f45c9c2953eccfa69cf5f3882`
- [Image build and mounted-volume rehearsal](https://github.com/open-covenant/covenant/actions/runs/39085355284) passed. The exact image booted under a 512 MiB gateway limit on an isolated network, preserved identities and journals across restart, and recorded zero jobs or paid dispatches. Optional provider/operator workers were disabled during this rehearsal; it does not claim their full-runtime memory qualification.
- Anonymous registry reads verified the manifest/config digests and source label;
  all 15 layer blobs were readable. The live gateway reported the same source
  revision before paid acceptance.
- The configured customer/native model remains `claude-haiku-4.5`. The bounded
  provider sends the exact catalog ID `anthropic/claude-haiku-4.5` with a hard
  OpenRouter pin. The original reported model, requested/upstream IDs, route and
  upstream response digest are retained in the operator-signed result.

## Funded AgentKit acceptance — 2026-10-18

The installed AgentKit 0.10.4 `covenant_inference` action completed one real
Solana-mainnet purchase between 01:13:42 and 01:14:10 UTC through the public gateway
with ordinary system DNS. The action requested an HTML meeting-cost calculator;
it returned 5,301 text bytes, a successful native receipt and finalized payment.
The receipt's finish reason was `stop`, with 126 input and 1,473 output tokens and
7,322 ms of operator-reported execution time. End-to-end action time was 27.62 s.
These are measurements of this acceptance run, not latency guarantees.

| Evidence | Value |
| --- | --- |
| Client request | `765dd8ae-6e4d-4081-9b98-22c63f1f26b2` |
| Request SHA-256 | `a1d427f12556a6b203d4af9666a8f4f986108e9f11a69c4fafc2bda90d187162` |
| Native job | `9674ccc2-e340-4531-8bd5-4707e1a1a43d` |
| Result SHA-256 | `a162fc36d788ecef186100d673618a1870951e0d1ec18df511aafe7c05c56b39` |
| Customer charge | 0.050000 USDC |
| Gross native Compute charge (before marketplace fees) | 0.030000 USDC |
| Buyer reserve after native charge | 2.970000 USDC available from 3.000000 deposited |
| Customer payment | [Finalized transaction](https://solscan.io/tx/2vpMX8KUdGoqiMZFdZrvDiUWNAv4Z2KCKqHxG6PFZk8tMaJMaHdnaqPz7fvnrJMaYDTXyPxXJW8YnnAGRPzcP5SG) |
| Finalized slot | `458248110` |
| Transaction fee / compute units | 10,001 lamports / 43,642 units |

Independent read-only chain verification found an exact 50,000-microunit native
USDC decrease for payer `7rozFdk421fc7DT3MpPJnD4ndXY81hBY6DzmKauZgzTB` and matching
increase for merchant `52Ca4rDuLd4RuxA2sZC7GJoGVnPrE77Z6nXEFDTnvtqw`, with no
transaction error. The mint was `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`,
six decimals. The customer transfer is separate from native operator accounting;
the quoted spread is not a claim of net revenue or an immediate operator withdrawal.

An independent offline check of the retained completed response verified the
buyer envelope signature, approved operator signature, request/model/generation
binding, receipt job hash, exact output-byte hash and returned text. The signed
provider metadata reported `openrouter` and `centralized`, matching the hard route
pin. A receipt binds the operator's result and provider assertions; it does not
independently attest model weights or certify generated application correctness.

## Failed attempts and retained accounting

Before success, an authorization using the official client's 20,000-unit default
failed simulation before admission. The full request-bound memo required 43,642
units; the corrected client and verifier use a fixed 60,000-unit ceiling. Its old
authorization expired without a historical transaction or customer balance change.
The original client reservation remains retained; it was not rewritten or reused.

A later native job failed strict model validation because the provider returned
the exact prefixed OpenRouter ID. No customer settlement occurred. The original
failed job and authorization remain retained; chain checks found the authorization
expired and no customer debit. Its deployed-provider reservation of 315,360
micro-USDC was explicitly abandoned as a full conservative loss, permanently
counted against both provider ceilings. This is not a claim of actual provider debit
or refund. No journal, monetary ceiling or failed request identity was reset.
The separate local qualification losses above remain in their original ledger.

## Production restart and original-authorization replay

A controlled Render restart replaced the serving instance after the purchase.
At 01:17:33 UTC the original signed authorization was POSTed to the server again,
while its completed response remained within the five-minute retention window.
The server returned HTTP 200 in 182 ms with the same canonical response, native
job and settled transaction. This was a real server replay, separately from the
installed client's local cached replay, which also returned identical evidence.

| Runtime observation (UTC) | Before restart 01:15:21 | After restart, before replay 01:17:18 | After replay 01:17:39 |
| --- | ---: | ---: | ---: |
| Gateway entries / complete / failed | 2 / 1 / 1 | 2 / 1 / 1 | 2 / 1 / 1 |
| Committed native reserve (micro-USDC) | 30,000 | 30,000 | 30,000 |
| Remaining native reserve (micro-USDC) | 2,970,000 | 2,970,000 | 2,970,000 |
| Gateway settlement transaction / authorization IDs | 1 / 1 | 1 / 1 | 1 / 1 |
| Provider calls | 5 | 6 | 6 |
| Provider accounted amount (micro-USDC) | 326,771 | 326,829 | 326,829 |
| Unresolved provider calls | 0 | 0 | 0 |

Restart caused one explicit operator qualification call, conservatively accounted
at 58 micro-USDC. The customer replay caused no new provider call, native charge,
authorization or customer payment. The provider totals include the permanently
retained 315,360-micro-USDC conservative loss; they are ceiling accounting, not
proof of actual upstream debits. Original identities, completed payment state and
failed-job tombstone survived restart. Result-body retention is bounded; financial
replay records are retained after the body expires.

## Published client package

The [v0.1.0 client prerelease](https://github.com/open-covenant/covenant/releases/tag/codex-compute-x402-client-v0.1.0)
was published on 2026-10-18 at 01:18:45 UTC against the accepted source revision.
The tarball and checksum asset were anonymously accessible and verified:

- [Installable client tarball](https://github.com/open-covenant/covenant/releases/download/codex-compute-x402-client-v0.1.0/covenant-compute-x402-client-0.1.0.tgz)
- SHA-256: `d38eddc61c035255744e8ac8193033daf4015fd191227f117b6f6476b7ac08a6`
- [Published checksums](https://github.com/open-covenant/covenant/releases/download/codex-compute-x402-client-v0.1.0/SHA256SUMS)

The Node24 package is distributed as an immutable GitHub release asset. An npm
registry publication is not claimed. Its installed contents are the same client
that completed paid acceptance and verify the additional signed provider metadata
without a package replacement.

## Public launch page and sharing metadata

[Inference with Coinbase AgentKit](https://opencovenant.org/compute-inference)
was live at website revision `b650d30c06563a90e92c9ada70a8bc958548f45d` on
2026-10-18 at 01:25:40 UTC. Browser inspection confirmed the shared Covenant layout,
verified-purchase example and install instructions. The anonymous public release
gate passed at 01:26:21 UTC, including the published client, acceptance/deployment
evidence and sharing image.

An independent `Twitterbot/1.0` request at 01:26:55 UTC received HTTP 200, the
correct canonical URL, matching Open Graph/Twitter titles, `summary_large_image`,
and a public 1200×630 PNG at [the advertised image URL](https://opencovenant.org/og/compute-inference.png).
The image returned HTTP 200 with `image/png`, 94,882 bytes and SHA-256
`2dc0c45f75c566faa099d4806e13f177a7c65d9484a5d65063508d70213e44c5`.
This verifies the deployed crawler-facing metadata and image; X controls its own
cache and final rendering. Launch-video rendering/delivery is recorded separately;
this record does not claim that an X post was published.

The standalone inference image remains pinned to the accepted `e52e2a790` digest.
The coordinator is a separate Render image service pinned to
`sha256:f7b747e294cb8f6f098d034e47c451edd9eb528ae2b7c5145ec6a46d8904e516`.
Source integration may build additional commit-tagged images, but it does not
promote them or replace either accepted runtime image.

Deployment and budget allocation: [release runbook](../deploy/compute-x402/README.md).
