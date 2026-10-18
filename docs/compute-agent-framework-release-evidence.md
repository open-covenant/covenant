# Microsoft Agent Framework inference release evidence

This release adds a Python x402 V2 payment client and an inference tool for
Microsoft Agent Framework. It continues adoption package 5. It is not a complete
Agent Framework model backend: the accepted gateway supports text inference,
and the separately configured planner chooses when to call the Covenant tool.

## Release state

Implementation, local qualification and funded acceptance passed on 2026-10-18.
Immutable package publication and public page/card deployment are still pending;
this record does not yet claim a public release.

The client-only release builds on Compute deployment branch
`b2662878a4c4aaa10ee6dcd3a05a3ef1e22615de` and the accepted x402 gateway
`e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`. It changes no server economics,
operator staking requirements, settlement program or active service image.

## Qualified package

`covenant-compute-x402` version `0.1.0` requires Python 3.11 or later and was
qualified on Python 3.12.14. The persistent workflow example uses a process lock
on Linux/macOS. Its optional Microsoft integrations are pinned to
`agent-framework-core==1.22.0` and `agent-framework-openai==1.17.0`, with the
official `FunctionTool`, `Agent`, `WorkflowBuilder`, `FileCheckpointStorage` and
`OpenAIChatCompletionClient` APIs.

| Asset | SHA256 | Bytes |
|---|---|---:|
| `covenant_compute_x402-0.1.0-py3-none-any.whl` | `0613775ced5316871b2248fd9942b523cee548e8e53e8222e8db614165023325` | 26,945 |
| `covenant_compute_x402-0.1.0.tar.gz` | `6606fbb919acb5e9dcb4a45114eb26a9235ea08a08d1d3f9711e642161ce9eb7` | 39,705 |

The same frozen wheel was installed for funded acceptance. All eight packaged
module files match the qualified source bytes. The source archive includes the
test helpers, native receipt fixture and exact qualification dependency lock.

## Local qualification

All 71 source tests and all 71 tests against the installed wheel passed. The
wheel was installed into a clean environment and tested outside the checkout;
`pip check` reported no broken requirements. The suite comprises 44 client tests,
seven cross-language tests and 20 Microsoft Agent Framework tests.

Coverage includes:

- ECMAScript-compatible canonical request bytes, Unicode key ordering, numeric
  serialization and SHA256 vectors checked against the accepted Node client.
- A native Rust signed receipt fixture, exact signed output bytes, request and
  quote binding, and rejection of substituted output or untrusted identities.
- Shared Node/Python SQLite scope, process contention for one cumulative budget,
  Node pending/completed recovery, unchanged legacy 20,000-unit authorizations,
  and Python completed-record replay in Node.
- Pending/unknown outcome recovery with the original authorization; changed
  prompts, insufficient funds or budget, incorrect chain/asset/recipient/memo,
  malformed JSON/UTF-8, and modified unsigned transactions refuse paid dispatch.
- Offline cache verification against current trusted keys, with rejection of
  altered cached text and no network or signing on a valid completed replay.
- Strict prompt-only official framework registration, persistent workflow
  phases, concurrency guards and the official chat-completions planner adapter.

A Python-created authorization also passed the actual Covenant facilitator's
official SVM verifier with generated unfunded keys and no broadcast. A dedicated
wire test confirms that payment terms preserve JSON integer types for the Rust
gateway's exact comparison. These checks establish local compatibility; the
funded acceptance below supplies separate production evidence.

## Funded acceptance

The installed package ran a model-selected incident-review workflow against the
existing production inference gateway on Solana mainnet. Planning made one
separately metered planner request and saved the selected tool call before any
customer quote, signature or payment dispatch.

| Bound identity or result | Accepted value |
|---|---|
| Request / quote ID | `eca14dce-99db-4a1a-9597-6b17402f094e` |
| Request SHA256 | `eb2737c40bb9cf1132b903466e0a8b35ecff4b3e5ac7f49828f1e2199a633c39` |
| Native Compute job | `b3ec6d09-7087-4a60-80a2-4d1898e8dcac` |
| Customer charge | 0.05 USDC; exact payer/merchant changes of −50,000/+50,000 micro-USDC |
| Native Compute price | 0.03 USDC gross before existing marketplace fees |
| Payment | [`5vqFrcJq…1ZmwPj92`](https://explorer.solana.com/tx/5vqFrcJq25P7hDmdfuivuvsN7o8EfGSv53s4anttMoDUzf5LvVLze6CQQ4vNcxWn5r3kk1attctt2iVP1ZmwPj92) |
| Chain confirmation | Finalized; slot 458553260 |
| Sponsored transaction execution | 43,642 compute units; 10,001 lamports in fees |
| Signed output SHA256 | `6f4d6670a34b772230df3d3dd804e834d1f6eb01816673e603614044486b082a` |
| Returned text SHA256 | `d6ac7b1099c3b3cf0effd079fb525b4f1e59daf3d3cedece72aed1f045132e45` |

The first purchase returned HTTP 202/pending. A fresh purchase process reused the
same saved UUID and authorization, then returned the completed result. Across
both attempts there was one quote and one wallet signature. The independent
JavaScript verifier checked the native signed proof and the exact authorized
transaction message against finalized chain data, including the USDC transfers.

A fresh-process workflow replay returned the identical purchase, job, payment
and text. All six measured counters were zero: wallet signatures, gateway quote
posts, gateway paid posts, RPC calls, planner dispatches and planner cache reads.
There was no additional customer charge. Finishing the review made one separate
planner request and no further Covenant purchase. The two planner calls together
added 6,192 micro-USDC of metered provider usage under the existing planner cap.

The existing customer journal and ceiling were retained. Its cumulative reserved
amount increased from 250,000 to 300,000 micro-USDC for this one purchase, with
four completed purchases and two older uncertain authorizations still retained.
No old reservation was deleted or recycled, and no new wallet, reserve or service
was introduced for this acceptance.

## Output-quality caveat

The raw generated incident review is retained without editorial correction. It
calls the approximately 13-minute detection-to-recovery interval a time to
rollback completion, although the supplied timeline places rollback eight
minutes after detection and recovery five minutes later. It also describes an
estimated 7.8% failure rate where the supplied observations show a 7.8 percentage
point increase from 0.2% to 8%; they do not establish an aggregate failure rate
over the incident window. This acceptance verifies the payment, signed output
and recovery path, not the correctness of that analysis.

## Acceptance requirements

- Install the built wheel into an isolated environment and run its actual
  framework tool and persistent workflow, rather than source imports alone.
- Check canonical request bytes and signed result verification against the
  existing JavaScript implementation and retained native receipt fixtures.
- Share the existing customer SQLite journal and scope, preserving cumulative
  reservations, completed purchases and uncertain authorizations.
- Record a model-selected tool call before purchase, the exact paid request,
  verified signed result, finalized Solana payment and fresh-process replay.
- Exercise changed-prompt, altered payment identity, insufficient-funds and
  insufficient-budget controls without paid dispatch.
- Publish immutable wheel/source assets and their checksums, then verify anonymous
  downloads, the public page, mobile layout and generated sharing image.

The original aggregate 10-USDC approval remains the outer live-test limit.
Customer payments and local planner calls keep their original, lower cumulative
ceilings and journals. An uncertain outcome cannot acquire a fresh purchase ID.

## Evidence interpretation

A receipt authenticates the named operator's output and its binding to the job;
it does not establish answer accuracy or independently attest model weights.
The sample incident analysis is advisory and cannot deploy a change, restart a
service or contact an incident team. Compatibility with the open-source Microsoft
Agent Framework does not imply Microsoft endorsement or partnership.
