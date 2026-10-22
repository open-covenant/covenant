# Batch inference release qualification

Status at 2026-10-21 20:48 UTC: **production enabled and paid acceptance verified**. One two-child batch settled
and its original saved response completed the client journal without another
network request or signature. Public batch SDK publication and discovery remain
separate. See the [sanitized release record](compute-evidence/inference-batches/2026-10-21/release.json).

The deployed code revision is `8ed36c4fe075bb097e90bfe8a009ca638083f0f3`;
later evidence-only documentation commits do not change that image provenance.
It is based on deployment commit
`5146e46e6bae31bf6813ca85f099674e878a1ae9`, includes the standard-wallet fix
`5fefb03d8eed2c1b48acfb8042cf8fd0a101cb55` and live Base support
`98a986cfadd0711c182143d90f959b705763ad5c`, and implements the
[bounded batch contract](compute-inference-batches.md). Fresh batches contain
one or two independent text requests. The generic 32-item proof/recovery format
does not activate larger fresh purchases.

## Local qualification

The combined source passed the checks below after integrating the
standard-wallet and Base updates. Exact-source CI, the immutable image, anonymous
Linux image retrieval and isolated persistent-volume restart smoke also passed.
Earlier batch images must not replace the current production image: independent
branches used journal v5 for Base terms and batch execution. The combined runtime
uses v6 and retains both, including frozen memo bindings, exact selected payment
rail, the standard client settlement wait and original financial records. Base
remains available for single requests; initial batches offer Solana only.

| Check | Result |
| --- | --- |
| Gateway | 74 unit tests and 1 binary startup/recovery test passed; mixed v5 Base/batch migration preserves original terms, selected rail and jobs |
| Protocol | Unchanged from the original batch candidate: 308 tests passed, including ordered proof vectors |
| Rust quality | Gateway/protocol all-target Clippy with warnings denied; formatting passed |
| JavaScript SDK | 111 tests passed with the real official SVM verifier integration enabled, including short and long batch memos |
| Python SDK | 172 tests passed, including Node/Python shared-journal recovery with both memo forms |
| Client packaging | Installed unpublished JavaScript 0.4.0 and Python 0.3.0 candidate archives match the deployed SDK trees; all 13 JS and 9 Python runtime files verified, with signed two-child import smoke |
| Payment verifier | 45 tests passed: Solana batch pricing/memo binding and retained Base verification/finality/recovery |
| Discovery, runtime and smoke harness | 42 tests passed, including both-rail single-chat discovery and Base credential isolation; batch discovery remains separate |
| Repository guards | Passed; regenerated metrics and corrected the base branch's existing Knowledge-image knob inventory omission |

The wallet and operator keys used in these tests are ephemeral and unfunded.
Coordinator results and chain finality are fixture data. Actual SDK signing and
official x402 verification are exercised; live provider execution and blockchain
settlement are not established by these tests. Those local tests did not change
production identities, latches, reserves, provider budgets or financial journals. The separately authorized live test below
records its actual changes.

The inherited CLI packaging check also needed a PR-only snapshot mode: the
deployment branch has a 0.5.0 node alongside 0.4.1 buyer/staking tools. Snapshot
archives validate each extracted binary against its own manifest and include
source and binary provenance. Tag releases still require aligned versions and
release notes; this change does not publish or rename an official release.

## Production acceptance

The immutable image
`ghcr.io/open-covenant/covenant/compute-node-public@sha256:5c55c1d79d6d65730b5eda683bece27f0ee86c6d977edc91e0129fe72fc6a466`
finished deployment at 20:35:55 UTC on 2026-10-21. The existing volume, identities,
57 environment values, Base single-request support and full financial histories
were retained; enabling batches added one environment value. The gateway migrated
v5 to v6. Push CI and the image used the exact deployed source; PR checks and the
four CLI snapshot builds used the recorded synthetic merge. All expected checks
passed, and all 16 public image layers were verified anonymously.

One installed-client purchase used two Haiku text prompts with a 256-token cap
and explicit `model_tariff_v1` pricing. It returned `A` and `B` in order. The single
retail payment was 28,608 micro-USDC; each signed native child cost 11,920, including
its 1,192 protocol fee and 10,728 operator net. All three original transactions
finalized, the two native escrows released, and the ordered RFC6962 root and every
signed child receipt verified independently in JavaScript and packaged Python.
The observed paid POST interval was **6,084 ms**, from the durable intent at
20:37:06.005 UTC to saved HTTP 200 at 20:37:12.089 UTC; this is one observation,
not a throughput or latency guarantee.

Provider accounting increased by 160 micro-USDC across exactly one 2,048-token
startup probe and two 256-token children. Total accounted allowance use was
**52,608 of 750,000 micro-USDC**, including provider usage, retail payment and
native gross once each. Provider planning reservations and native net/protocol
fees were not added again. Finalized transaction fees totaled **20,001 of 50,000
lamports**, with no rent or new token accounts. This verifies local provider
accounting, not a provider invoice. The permanent prior provider loss remained
retained, and no unresolved provider call or native settlement remained.

The private acceptance harness stopped after saving HTTP 200: its paid-post
`sequence: "0001"` field overwrote the numeric event sequence, so the next log
append raised `event_log_corrupt`. The original paid authorization, response,
financial reservation and event file were preserved. This was a harness logging
failure after successful remote settlement, before the SDK cached completion.
There was no blind retry, replacement quote, second signature or new request ID.

A separately pinned recovery helper verified the saved response, original signed
payment and captured finalized transaction, then passed only that saved evidence
through the installed SDK's normal journal claim/complete/release path. It made
zero external calls and sent no paid POST. The original frozen runner then passed
completed-cache replay at 20:44:11 UTC with every call counter zero. The frozen
packaged Python verifier independently verified the actual completed cache at
20:44:49 UTC with zero network calls and no journal changes. All 12 prior
customer rows remained identical; all 13 total journal rows remained unchanged
before and after replay. The temporary admission hold was then archived after
review without a restart, new payment or financial write. All five original
processes remained running; public health at 20:48:25 UTC reported the exact
deployed revision with admissions enabled.

## Remaining release gates

1. Separately publish immutable batch-enabled SDK versions. The accepted private
   archives retain base version labels 0.4.0/0.3.0 and are not public batch releases.
2. Decide public batch discovery/indexing. Existing model catalog, OpenAPI and
   explorer discovery continue to advertise the accepted single-chat product.

The optional production restart and direct server replay were not run. Isolated
image restart smoke and zero-call completed-client replay passed; these are
separate evidence claims. No new purchase, replacement authorization, automatic
retry or further spending is authorized by this release record.
