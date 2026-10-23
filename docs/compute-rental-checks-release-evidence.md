# Compute rental checks: release evidence

**Production rental and scheduled operation accepted on October 19, 2026.**

A real NVIDIA L40S rental completed authenticated SSH, GPU inspection, on-chain
settlement, buyer balance reconciliation and provider cleanup. A subsequent
restart enabled the six-hour schedule without creating another rental or
changing the two resolved budget entries. The public feed reports the next
window at **06:00 UTC on October 19, 2026**. The earlier failed paid attempt
remains in history.

The successful acceptance rental was manually started. This record establishes
that scheduled mode is enabled and survived its restart; it does not claim that
the next scheduled window has already run. The machine-readable record is
[`acceptance.json`](compute-evidence/rental-checks/2026-10-19/acceptance.json).
Website deployment is verified separately after this evidence is published.

## What the release checks

The scheduled pilot uses the released buyer CLI to fund one bounded GPU lease,
authenticate its SSH host identity through provider evidence, inspect the GPU,
close the rental, verify settlement on Solana, reconcile the buyer balance and
confirm provider instance deletion. A passing result requires every step.

This is a point-in-time check of one rental. It does not certify every operator,
establish hardware attestation, or guarantee that another rental will succeed.
A preflight result, an interrupted run, a refund or completed recovery is not a
passed GPU check. An unavailable provider and an empty capacity result are
reported separately.

## Source and artifact bindings

These versions serve different roles and must not be collapsed into one
"deployed revision."

| Component | Pinned source or artifact | Scope |
| --- | --- | --- |
| Rental-check implementation | [PR #348](https://github.com/open-covenant/covenant/pull/348), [`8ee0a9c48ec68d475065612d439bd24a37751ebc`](https://github.com/open-covenant/covenant/commit/8ee0a9c48ec68d475065612d439bd24a37751ebc) | Runner, public state, one-attempt buyer submission and authenticated provider evidence. |
| Alert delivery and observer overlay | [PR #349](https://github.com/open-covenant/covenant/pull/349), [`1bb226f0404d49f852a726154a13e91f16437422`](https://github.com/open-covenant/covenant/commit/1bb226f0404d49f852a726154a13e91f16437422) | Verified destination handling, private key file permissions and observer-only image packaging. |
| Coordinator readiness correction | [PR #350](https://github.com/open-covenant/covenant/pull/350), [`9e224b6b8f21c8c18b2224048dd1b726e3e20b5b`](https://github.com/open-covenant/covenant/commit/9e224b6b8f21c8c18b2224048dd1b726e3e20b5b) | Deployed for the first paid acceptance attempt; its failed result is retained below. |
| Host-log readiness and guarded acceptance retries | [PR #351](https://github.com/open-covenant/covenant/pull/351), [`5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9`](https://github.com/open-covenant/covenant/commit/5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9) | Deployed for the passed manual rental below. The public health endpoint reports this revision; final artifact and rollout evidence is being bound separately. |
| Released client | [Compute 0.3.1](https://github.com/open-covenant/covenant/releases/tag/compute-v0.3.1), source `8ee0a9c48ec68d475065612d439bd24a37751ebc` | Published, non-prerelease and immutable. The tag is not moved for deployment-only corrections. |
| Rental broker | Source `8ee0a9c48ec68d475065612d439bd24a37751ebc` | Separate service; image digest `sha256:7b17bc68f85735a8216b3a6ba92efe548b984274a7baf5d2fb0c629074bab9e8`. |
| External observer | Source `1bb226f0404d49f852a726154a13e91f16437422` | Observer and supervisor files layered over the accepted inference image; overlay digest `sha256:d95c659e209829640c67ff7930fcfd66b2bd330c5e8c50f2542e6e155114a1ac`. |
| Preserved inference runtime | Source `e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`; base image digest `sha256:aef10b2661b20d35202ffdbc68ab3efb249c8e9f45c9c2953eccfa69cf5f3882` | Existing payment binaries, provider bridge and dependencies are preserved. The observer image source is not the binary's `/health` revision. |

The immutable client release was published on 2026-10-18 and contains 15 assets:
four platform archives and their signatures/certificates, plus signed checksum
files. Publication metadata alone is not installed-client acceptance. The acceptance record retains the actual archive hash and installed CLI version
used by the rental, along with the separate coordinator, broker and observer
image digests.

## Budget and prefunding

New rental commitments are capped at **$2 in a rolling 24-hour window and $20
for the pilot**. These are admission limits, not a guarantee that an external
provider's invoice cannot exceed its allocation during a deletion outage.
Uncertain outcomes retain their full reservations and block a replacement
rental. Recovery reconciles the original job; it does not submit a new one.

The provider allocation includes a 16 GB transfer planning allowance. This is
not an enforced transfer quota. The operations guide records the lease ceiling,
provider price ceiling, readiness deadline, fee/storage allocations and teardown
assumptions. Operator revenue is not subtracted from the pilot's spending.

The dedicated buyer was prefunded with **8 USDC** in finalized transaction
[`2FGjQG9j…Y1td3mr`](https://explorer.solana.com/tx/2FGjQG9jxSMLcTwktpSHWV8zf2PyqUja4FQUiuWjM8HAMCYEsbmu7Du17geKSMR9KTeB8RuQAugXFB4N5Y1td3mr).
This is funding evidence, not a completed rental, an observed rental charge or
permission to exceed the admission caps. Unused escrow returned as coordinator
buyer credit must not be described as another on-chain refund transaction.

## Scheduling and alert evidence

The configured rental windows are 00:00, 06:00, 12:00 and 18:00 UTC. The local
monitor counts missed or unsuccessful six-hour windows; three consecutive
windows reach its alert threshold. A recent reporting heartbeat is independent
of the age or verdict of the latest rental.

The external observer runs on the separate existing inference instance. It
checks the public feed every 60 seconds and records an outage after **18
continuous hours** without a fresh heartbeat. Fresh intentional pauses remain
visible; stale disabled reports do not conceal a reporting outage. Its durable
outbox retains exact event IDs across uncertain delivery outcomes and restart.
A reporting-restored notice does not claim that a GPU rental passed. Both
services still share Render infrastructure, so a correlated platform outage can
prevent observation.

Alert evidence records only whether Cloudflare explicitly accepted the exact
configured destination as delivered or queued. **Provider acceptance is not
confirmation that the message reached or was read in the inbox.** The verified
mailbox, credentials and raw provider responses are private. The public record
must not expose them.

## Qualification completed before live acceptance

- The follow-up passed 22 Python alert tests and 36 observer, x402 runtime and
  smoke-harness tests, including recipient substitution, malformed responses,
  outage timing, durable retries and worker isolation.
- Both Compute documentation validators, shell syntax, workflow YAML and
  embedded script checks passed.
- Anonymous manifest/config inspection bound the accepted inference image to
  source `e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`. The observer build separately
  verifies unchanged runtime bytes and exact copied observer source, and runs a
  mounted-volume rehearsal with paid admissions disabled. Final image results
  and hashes belong in the acceptance JSON.
- The page uses the shared Covenant layout and generated X card. Its concept
  illustration represents the check sequence; it is not an actual GPU
  photograph or verification record. Website deployment is checked separately.

Local tests, image publication, initial deployment, startup recovery, preflight
and prefunding do not replace the live checks below.

## First paid acceptance result — failed, settled and deleted

The coordinator readiness revision `9e224b6b8f21c8c18b2224048dd1b726e3e20b5b`
was deployed in one-shot mode for this attempt as Render deployment
`dep-dbam8rs9v7es7392pb0g`, image digest
`sha256:046b7ce0bcea4b9814452908201a699b6b0ef0a828eec6a57d06b53a2c9682f8`.
This identifies the deployment used for the failed attempt. It must not be
relabeled as the later correction.

The first boot-triggered one-shot run, identified by prefix `c76d6cc5`, was
blocked by unavailable capacity before creating a job or reserving any spend.
Its blocked history and supervisor markers are retained. A later read-only
preflight found one eligible capacity entry. It did not itself purchase a lease.

One explicitly authorized manual `--once` invocation then opened the following
acceptance attempt under the existing pilot budget:

| Field | Recorded value |
| --- | --- |
| Run ID | `2159d86f-ce2f-43ef-9e94-c3fc1d921a5f` |
| Job / lease ID | `44c56847-1e0f-4e5c-98d9-4a5724c9efc1` |
| Started | `2026-10-19T00:31:52Z` (`1792369912` Unix seconds) |
| Admission reservation | `491240` micro-USD ($0.49124) |
| Finished | `2026-10-19T00:32:38Z` (`1792369958` Unix seconds) |
| Installed buyer | `covenant-compute 0.3.1` |
| Failure point | Host-identity check; exact subprocess cause was not retained |
| Original recorded reason | `subprocess_failed` |
| Terminal recorded stage | `provider_cleanup` |
| Final verdict | `failed` |
| Buyer charge | `13010` micro-USDC (0.01301 USDC) |
| Unused escrow credit | `107230` micro-USDC (0.10723 USDC) |
| Settlement | `verified_onchain` |
| Buyer balance reconciliation | Verified |
| Provider instance deletion | Verified |
| Amount still counted against the pilot | `384010` micro-USD ($0.38401) |

The failed verdict and original `subprocess_failed` reason are preserved. Cleanup
advanced the terminal recorded stage to `provider_cleanup`; it did not convert
the attempt into a success. The record confirms settlement, exact buyer balance
reconciliation and authenticated deletion, but does not confirm the SSH host key
or a successful GPU probe.

A separate diagnostic after deletion received provider API acceptance for a log
request, followed by HTTP 403 for the log download. The original failing
subprocess error was not retained. That later diagnostic does not establish the
cause of the earlier failure, and the result must not be rewritten as a proven
log-readiness race.

The settled charge replaced only the buyer ceiling in budget accounting. The
provider, network-fee and storage allocations remain counted, leaving $0.38401
against the pilot. This accounting total is not an observed provider invoice.
The failed attempt and its counted amount must survive the next rollout and
acceptance attempt.

## Successful manual rental

A second authorized manual check completed on coordinator revision
`5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9`. The recorded coordinator image is
`ghcr.io/open-covenant/covenant/compute-coordinator-public@sha256:e702d919cb16c91d190398e28906052962ae963452f314024534c38738d93637`;
its anonymous download and complete layer-hash verification are retained in
[`artifact-verification.json`](compute-evidence/rental-checks/2026-10-19/artifact-verification.json).
The same file binds the broker and observer images to their distinct sources.

| Field | Recorded value |
| --- | --- |
| Run ID | `74499d51-acd1-450d-bbec-5f97b4fe7b61` |
| Job / lease ID | `1e8517f4-737e-470b-80c6-5f83e0ba1003` |
| Started | `2026-10-19T01:03:46Z` (`1792371826` Unix seconds) |
| Finished | `2026-10-19T01:04:46Z` (`1792371886` Unix seconds) |
| Final verdict | `success` |
| Inspected GPU | NVIDIA L40S |
| Authenticated SSH identity | Verified |
| GPU probe | Verified |
| Settlement on Solana | Verified |
| Buyer balance reconciliation | Verified |
| Provider instance deletion | Verified |
| Buyer charge | `17766` micro-USDC (0.017766 USDC) |
| Unused escrow credit | `102474` micro-USDC (0.102474 USDC) |

The public endpoint returned this result with all five verification flags true
at `2026-10-19T01:06:45Z`. The exact response is preserved in
[`public-feed-at-success.json`](compute-evidence/rental-checks/2026-10-19/public-feed-at-success.json),
SHA-256 `f2498d624c75f6a81dbc5402f933939c5273d2cd83e6b44edbb6f00d2f0928a6`.
It returned HTTP 200 with `Cache-Control: no-store`. A separate health read
reported `status: ok` and the coordinator revision above. A subsequent read-only capture at `2026-10-19T01:08:12Z` bound this result to
Render deployment `dep-dbamnbqjnfac738ao1fg`, the image above and installed client
`0.3.1`. The Linux x86-64 archive SHA-256 is
`37cc3946c5d2f6a87a911138e6cc162ddeb2b6b8b31490f43efa100dcb261eb1`;
the installed binary SHA-256 is
`13a33fd605f81344d462f01b12fb1d856af91bf88dbed2e9cab991c24d9f1652`.
The capture found no active rental, no pending entry and a stable journal during
the read. It preserved both resolved budget entries.

The released client returned `verified_onchain` for settlement transaction
[`2tZEJ8tS…Du3iRMH`](https://explorer.solana.com/tx/2tZEJ8tSdoRPh4bMjfJnTFXqaYVsqV9UWZTL91X3hwLa3x2i6SGn3fXzh2r1pGYUXxtUqTfuFtcEQhvVFDu3iRMH).
Authenticated provider readback for this exact job reported its binding verified
and the instance absent. The sanitized fields are retained in the acceptance
JSON; private SSH access, account configuration and raw logs are omitted.

The same feed retained the first paid failure, five earlier preflight results
with no job or recorded charge, and `772776` micro-USD ($0.772776) counted against
both the rolling daily and pilot limits. These budget totals include conservative
allocations; they are not an observed provider invoice. The previous failed
rental's $0.38401 remains included.

This capture had `feed_status: disabled`, `enabled: false`, `is_stale: false`
and `next_due_at_unix: null`. It proves that a current report can contain a
passed manual rental while scheduled admissions remain disabled. The subsequent scheduled-mode capture below records that separate gate.

## Copied-state restart rehearsal

The supervisor was replayed against copies of the completed live state in both
`once` and `scheduled` modes. Both replays made zero paid dispatches, recovery
dispatches, real child processes and real network calls. The copies and original
state were unchanged; both budget entries remained resolved. The
[rehearsal record](compute-evidence/rental-checks/2026-10-19/copied-restart-rehearsal.json)
records its scope: bootstrap, process creation, network and monitoring were
mocked. This qualifies admission behavior on copied state; a real scheduled-mode
restart is a separate gate.

## Scheduled deployment and live restart

The same coordinator image was promoted to scheduled mode as deployment
`dep-dbams6uq1p3s73c9umtg`. A live post-restart read at
`2026-10-19T01:12:56Z` found the installation, budget, latest result and supervisor
journal hashes unchanged. The buyer balance was unchanged, both budget entries
remained resolved, and there was no active rental or pending entry. No duplicate
dispatch was observed.

The public feed was fresh, `enabled: true`, with a six-hour cadence and
`next_due_at_unix: 1792389600` (06:00 UTC on October 19). Its exact captured JSON
is [`public-feed-scheduled.json`](compute-evidence/rental-checks/2026-10-19/public-feed-scheduled.json).
The prior disabled capture remains available to distinguish the manual check
from schedule activation.

The separate observer recorded a fresh probe at `2026-10-19T01:12:12Z`, with no
open outage incident or pending alert. The broker reported no accepted jobs
awaiting completion, no pending outbox entry and zero unpaid USDC. Its five
credited jobs and 0.263351 USDC lifetime earnings include work outside this
pilot and are not presented as pilot revenue. The inference health endpoint
still reported the accepted runtime revision
`e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e` with admissions enabled.

A final routing test was accepted by Cloudflare for the configured verified
mailbox. Inbox arrival was not verified. The live acceptance did not wait for
an 18-hour outage or intentionally create one; threshold, retry and isolation
behavior were covered by tests. Private recipients and account configuration
are omitted from the public evidence.

## Accepted scope and remaining website check

| Evidence | Result |
| --- | --- |
| Released client and exact installed binary | 0.3.1 archive and installed binary hashes bound to the live rental |
| Authenticated SSH and GPU inspection | Passed on one NVIDIA L40S rental |
| Close, settlement and buyer balance | Verified, with linked Solana settlement transaction |
| Authenticated provider cleanup | Exact job binding verified and instance absent |
| Final verdict and retained failed history | Passed result persisted; previous paid failure and its budget accounting retained |
| Copied-state admission replay | Zero paid dispatches in once and scheduled modes; original and copied state unchanged |
| Real scheduled-mode restart | Same settled result, balances and journal hashes; no active or duplicate rental |
| Public heartbeat and next window | Fresh report, enabled six-hour schedule, next window 06:00 UTC |
| External observer and broker | Fresh observer without pending incident; broker idle with zero unpaid balance |
| Alert route | Cloudflare accepted the final routing test; inbox arrival unverified |
| Website and X card | Verified separately after evidence publication |

This is the evidence available at release acceptance. It is not a guarantee of
future capacity, provider availability or every scheduled run. The public page
shows subsequent verdicts, budget pauses and reporting freshness as they occur.

[Operations guide](https://github.com/open-covenant/covenant/blob/5f0f006aaca4150a79f5fb56f8b5f25ba3eacdf9/deploy/compute-canary/README.md)
