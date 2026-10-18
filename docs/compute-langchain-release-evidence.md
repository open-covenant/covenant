# LangChain and LangGraph inference release evidence

This release adds a LangChain tool to Covenant's existing Solana-USDC x402
inference client. It is a partial delivery of adoption package 5: the JavaScript
LangChain/LangGraph integration. Dedicated native-protocol TypeScript/Python
Compute clients and the other framework adapters remain separate work.
Compatibility does not imply a partnership or endorsement.

## Candidate and scope

- Package: `@covenant/compute-x402-client` version `0.2.0`, Node 24 or later.
- Candidate SHA-256:
  `77ef86146f982432f75a83d227afb8c7a8f36049bb23d4eba29ae0da0cb606cf`.
- The `./langchain` export returns an official `DynamicStructuredTool`, qualified
  with LangChain `createAgent` and LangGraph `createReactAgent`.
- The `./langgraph-demo` export provides a labeled sample support-ticket workflow
  using a real StateGraph and the official persistent SQLite checkpointer.
- The existing x402 client, payment journal, receipt verifier and AgentKit API
  remain unchanged. This release does not require replacing the accepted gateway
  image, coordinator or operator. Solana USDC settlement, CVNT staking and fees
  retain their existing behavior.

The model-facing tool schema accepts only a prompt. The application fixes the
model, generation limits, purchase UUID, wallet, deployment identities, spending
ceilings and optional system policy. One logical purchase retains its UUID across
graph attempts and process restarts. Changed request bytes under the same UUID
are refused. Pending or uncertain purchases retain their original authorization
and reservation; they are never retried under a fresh identity automatically.

The tool returns a compact answer/payment summary to the planner and retains the
full verified response in `ToolMessage.artifact`. The receipt binds the operator,
request and exact answer bytes. It does not prove the answer is factually correct,
independently attest model weights, or authorize account actions. Retained graph
checkpoints can contain the sample prompt and full signed evidence.

## Local and installed-package qualification

On 2026-10-18, all **32 source tests and 32 installed-package tests passed**, with
zero skipped tests. The installed candidate replaced only the package in the
previously clean consumer environment; pinned framework versions and dependencies
were retained. Every packaged file was compared byte-for-byte with the installed
package and source.

The checks cover actual framework registration and tool execution, exact signed
system-policy bytes, model-argument refusal, combined UTF-8 limits, receipt
rejection, durable concurrent recovery and authorization reuse. The support demo
runs planning, purchase, replay and finish in separate Node processes with the
official SQLite checkpointer. A pending HTTP 202 purchase cannot be presented as
finished; resumed execution polls the original purchase. Completed replay returns
the same artifact without another network request.

These local tests use an explicit scripted planner, generated unfunded keys and
a loopback gateway/RPC fixture. They exercise the official x402 facilitator but
do not broadcast a transaction or establish live-model acceptance.

The pinned framework packages are `@langchain/core` 1.2.18,
`@langchain/langgraph` 1.5.1 and `langchain` 1.5.18. Optional demo dependencies are
`@langchain/openai` 1.6.2 and `@langchain/langgraph-checkpoint-sqlite` 1.1.0. The
native SQLite binding was rebuilt and exercised on Node 24. Full local evidence
and earlier candidate qualification are recorded in the
[client implementation record](../sdk/compute-x402-client/IMPLEMENTATION.md).

To reproduce the complete source suite from `sdk/compute-x402-client` with
Node 24, build the optional native SQLite dependency before running the tests
and supply the facilitator path so its cross-component test is not skipped:

```sh
npm ci --ignore-scripts --no-audit --no-fund
npm rebuild better-sqlite3 --ignore-scripts=false
COVENANT_X402_BRIDGE_PATH="$(pwd)/../../services/compute-x402-facilitator/bridge.mjs" npm test
```

## Retained earlier candidate

The initial candidate, SHA-256
`4328500321a9bf4960980e5666d68f0f0be0c7f1bff904ccb2d2caaa48fcd33e`, completed the
financial purchase/recovery path, but its sample draft falsely claimed an account
had been flagged. That output was rejected for public demonstration. Its payment,
journal and qualification records were retained rather than rewritten or deleted.

The corrected candidate adds an immutable application-controlled system policy
to the signed request. The sample policy explicitly prohibits claims of account
access, escalation, refunds or other actions taken. A new acceptance purchase
uses a separate application-authorized UUID with the original wallet, shared
journal and unchanged ceilings. This is a distinct purchase after a known
completed outcome, not a retry of an uncertain charge. Instruction changes do not
guarantee factuality; the resulting draft still requires review.

## Funded acceptance — 2026-10-18

The installed corrected candidate completed a real LangGraph workflow through
the existing public gateway at `https://compute-inference.opencovenant.org`.
At 15:06:56 UTC, a live model selected `covenant_inference` using automatic tool
selection and supplied the support-triage request. The graph persisted its
checkpoint before the paid tool ran. The exact application-controlled drafting
policy was present in the signed native request.

The first paid submission returned HTTP 202. A later process resumed the original
pre-tool checkpoint and reused the same signed authorization; the wallet signed
once for this purchase. The completed result was accepted at 15:07:45 UTC. It
classified the labeled sample as Billing/High, requested evidence and produced
a draft for a human agent to review, without claiming that account actions had
already been taken. No customer was contacted or account action performed.

| Evidence | Value |
| --- | --- |
| Package SHA-256 | `77ef86146f982432f75a83d227afb8c7a8f36049bb23d4eba29ae0da0cb606cf` |
| Customer request | `ab510efd-d01e-45b1-b490-242b9d1a8549` |
| Request SHA-256 | `b861a35af8890a1d0b7bb234bed586c50fe95271e686b5f6f35d17419679e55c` |
| Native job | `ed4fc8c9-0781-40d1-b45a-3987e9ddc91f` |
| Signed output SHA-256 | `a17d6f509cfd2395735678eb7401e9ace3f93fa29ced2b7f1cfc799b245fd424` |
| Customer charge | 0.050000 USDC |
| Gross native Compute charge before marketplace fees | 0.030000 USDC |
| Customer payment | [Finalized transaction](https://solscan.io/tx/2UQMqsZvHs1o1XYocySKkRNX9h3dNLnvkeXSH9obBsaqSvZ3saneaoB83hkS5F8opEvz8bh269oWZH3EW5AX16QQ) |
| Finalized slot | `458478907` |
| Transaction fee / compute units | 10,001 lamports / 43,642 units |

Independent read-only verification at 15:08:25 UTC confirmed the transaction
signature and exact message, no transaction error, and matching native-USDC
deltas: payer `7rozFdk421fc7DT3MpPJnD4ndXY81hBY6DzmKauZgzTB` decreased by 50,000
microunits and merchant `52Ca4rDuLd4RuxA2sZC7GJoGVnPrE77Z6nXEFDTnvtqw` increased
by 50,000. The mint was `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`, six
decimals, on Solana mainnet. The original customer wallet retained 350,000
micro-USDC after acceptance.

The client verified the approved operator's native receipt and its binding to the
request, model and exact output. The receipt reported 318 input tokens, 224
output tokens and 3,550 ms of operator execution. The workflow deliberately
paused between phases; these measurements are not an end-to-end latency
benchmark or a factual-quality guarantee.

At 15:08:12 UTC a separate replay process re-executed the stored tool call and
returned the identical accepted purchase artifact with zero wallet signatures,
gateway quote requests, paid requests, RPC calls or planner calls. The final
planner then summarized the result at 15:08:22 UTC. Its phase performed no
customer payment or paid tool call. This is application/checkpoint restart and
client-journal replay evidence; no new gateway restart is claimed for this
release.

Planner requests had separate persistent limits. Their final lifetime ceiling
accounting was 613,573 micro-USDC, including 600,480 retained from earlier
uncertain qualification outcomes. This is conservative accounting, not a claim
of actual provider debits, and is separate from the 0.05-USDC customer purchase.
The shared customer journal retained four purchase entries and 200,000
micro-USDC of cumulative reservations, with three completed purchases. Previous
attempts and their accounting were retained; neither wallet identity, journal
nor spending ceiling was reset for the corrected run.

## Publication and public page

[PR #338](https://github.com/open-covenant/covenant/pull/338) merged the integration
at `75b50d60d72f9502850559f48371dd8f95c8b294`. The accepted package source is
`9e53e687b0ed483dca9b66328e70c43f0c9aecdf`.

The [v0.2.0 release](https://github.com/open-covenant/covenant/releases/tag/codex-compute-x402-client-v0.2.0)
is published with GitHub release immutability enabled. Anonymous HTTP downloads
of all three assets returned 200 and matched their recorded SHA-256 digests:

| Asset | SHA-256 |
| --- | --- |
| `covenant-compute-x402-client-0.2.0.tgz` | `77ef86146f982432f75a83d227afb8c7a8f36049bb23d4eba29ae0da0cb606cf` |
| `langchain-mainnet-acceptance.tar.gz` | `5da8fb7938c22f9a62520a324bbfbd974afea3ecef3b2a32725d367fcea38337` |
| `SHA256SUMS` | `029dc7287ad1adbbcb59f3184e18ee2c8aaccea0e90cbdd7f908f65fee1dab3a` |

The public package is byte-identical to the installed candidate tested above.
The acceptance archive contains the actual model-selected tool call, signed
purchase and output, checkpoint replay and independent finalized-payment check.
It contains no wallet key, authorization journal, provider credential or private
RPC configuration. The earlier immutable `0.1.0` AgentKit release is unchanged.

[PR #339](https://github.com/open-covenant/covenant/pull/339) published the
[integration page](https://opencovenant.org/compute-langchain). Render service
`srv-d7ud5idb910c73aov88g`, deployment `dep-dbae8k9srm7s73abshag`, reported
**Deploy succeeded | Live** for website revision
`096439598f8d22e4fdd6631ebbd88c9ab47ef8c0` at 15:25:32 UTC on 2026-10-18.
The production build passed its generated-card checks for 79 pages.

At 15:26 UTC, ordinary and `Twitterbot/1.0` requests both returned HTTP 200,
the correct canonical URL, `summary_large_image`, and the same generated
Open Graph/Twitter image URL. The PNG returned HTTP 200 at 1200 × 630, SHA-256
`3381b5382732daf1428448dca2e007adbc9765831c78f6404d42f03c4d3a000e`.
All six published evidence files matched the accepted local bytes, and the route
was present in the sitemap. This verifies the crawler-facing page and image,
not an actual X post or a guarantee about X's cache.

Production browser checks at 1440 × 1000 and 390 × 844 found no horizontal
page overflow or JavaScript console errors. Setup/workflow anchors, internal
code scrolling and the optional-dependencies disclosure worked at both sizes.

The live gateway still reported healthy admissions and its previously accepted
revision `e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`. This client release did not
replace that service or require another funded gateway reserve.
