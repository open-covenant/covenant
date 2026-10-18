# Kani workflow bootstrap provenance — 2026-10-17

The first workload develops Percolator proof amendments through Covenant Compute and verifies their exact source with native, receipt-covered Kani execution. The buyer workflow and publication gates are documented in [the buyer README](../agent-os/crates/covenant-compute-buyer/README.md#authorship-and-kani-verification-with-receipts).

## What was authored where

| Component or work | Provenance | Evidence/status at this checkpoint |
| --- | --- | --- |
| `deploy/kani-check` runner, tests and Dockerfile | Covenant agent authorship | Job `da35b756-1289-458e-8604-25072f0ddf8b`; signed output verified, checker accepted 39 tests; returned patch SHA-256 `64fa5cbeb83f162eff65cbeee41f10ef225d1da4491eafd20e7de16575ef29a4` |
| Typed Kani protocol, node execution integration, direct-check admission, buyer CLI and durable journals/budget, authenticated evidence transport | Local bootstrap engineering | Local implementation/tests; not represented as network-authored work |
| Original Percolator #239 engine fix and earlier route harness drafts | Pre-existing local work | No retroactive claim of Covenant authorship |
| Existing #241 proof rerun | Planned native verification workload | A verification receipt can establish execution provenance, not authorship of the original proof |
| Reduced #239 route-proof amendment | Prepared Covenant authoring order | Dispatch is held until #241 native verification is validated; final publication additionally requires independent green and buggy-base control receipts |

The paid runner's signed envelope offered 2,000,000 micro-USDC. The authenticated coordinator API reported a charge of 1,039,880 micro-USDC, while the signed receipt retained the 2,000,000 ceiling. The project ledger therefore kept the entire ceiling reserved pending stronger charge reconciliation. The API charge is not relabeled as a signed settlement statement.

Two earlier attempts reached model-spend caps and produced no paid patch: runner job `fc092cb8-63ef-4783-8105-72c530b7d8b2` and evidence job `c1fc8c33-ec80-47de-9480-8d7207ce891b`. Their signed terminal failures and reported zero charges were retained. A local protected-path validation failure occurred before submission and is separately recorded; it is not a network job.

## Gates for a Percolator amendment

1. Preserve the exact original PR head, new authoring task, source snapshot/tree mapping, accepted patch hash and full authenticated builder/checker evidence.
2. Verify the exact resulting source through a native `check` order. Preserve the Kani plan, requested resources, immutable image, solver versions, full raw artifact hashes/bytes and classified outcome under the operator receipt.
3. Apply the identical harness source to the pinned buggy base and require the named assertion to be refuted. Timeout, OOM, compiler errors, failed unwinding and missing covers are inconclusive.
4. Review the stated domain and successful route covers. A narrowed proof is described as narrowed; it does not inherit the larger draft's claimed domain.
5. Amend an external PR only when the source/patch/receipt chain and proof/control verdicts match. Publishing, deployment, network authorship, verification and on-chain settlement are separate facts.

This checkpoint records bootstrap provenance; it does not claim deployment completion, live Kani acceptance, external PR amendment or complete payer settlement. Later local compatibility/security repairs to the network-authored runner must be identified as local follow-up changes instead of attributed to its original paid patch.

## Subsequent live evidence

[The Percolator receipt ledger](compute-evidence/percolator/2026-10-18/README.md) records the completed paid runner authorship, native canaries, both accepted #241 proof checks and the earlier failed attempts. It preserves the distinction between original local work and subsequent Covenant execution.
