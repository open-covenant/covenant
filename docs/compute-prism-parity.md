# Compute: Prism adoption ledger

Observer release: [PR #323](https://github.com/open-covenant/covenant/pull/323),
merged at `28cff590ca5764dfddcc07d729f1e92ed5e8761e`. The release branch was
originally based on `deploy/compute-coordinator` at
`8e2faf72eab6d6b62c0afc643f1afb1bcc9cf30a` and incorporated deployment base
`b0d5910b60be2a4c370f356fdec6dc221c7a9378` before promotion.
The original adoption audit used Covenant `5b023331575da656da77edc8554213f58e99ad90`.
Comparison: Prism `main` at `4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec`.
The accepted public releases here cover packages 1, 2 and 6. Package 6 completed
funded AgentKit acceptance, production restart/replay, client publication and
public page/card checks on 2026-10-18. The broader `codex/compute-prism-parity`
implementation and overlap in independent deployment-base changes are not
acceptance evidence for the remaining packages.
Package 5 now has a separately released JavaScript LangChain/LangGraph
integration over the accepted x402 client. Funded acceptance, checkpoint replay,
immutable publication and public page/card checks passed on 2026-10-18 in its
[release record](compute-langchain-release-evidence.md).
The native Python client and Microsoft Agent Framework tool have separately
passed local and funded acceptance on 2026-10-18; immutable package publication
and their public page/card remain pending in the
[Agent Framework release record](compute-agent-framework-release-evidence.md).
This is partial package-5 coverage, not completion of the full SDK/framework set.

Packages 1 and 2 preserve Solana USDC settlement, CVNT staking, admission and
existing fees.
Provider market sampling is read-only and cannot create rentals or move funds.

| # | Earliest upstream change | Local implementation | Local validation | Production evidence |
|---|---|---|---|---|
| 1 | Aug 12 [GPU prices](https://github.com/prismnetwork-tech/prism/pull/142) | Durable hourly provider samples, provisioning quotes, bound settled rates, public page/API | 1,395 affected-crate tests + 280 protocol tests; local provider/restart rehearsal; renderer and docs checks | Live 2026-10-17; public image pulls, provider samples and restart acceptance verified |
| 2 | Aug 13 [observer](https://github.com/prismnetwork-tech/prism/pull/160), [status](https://github.com/prismnetwork-tech/prism/pull/164) | Read-only pinned chain observer, public status and reviewed incidents, durable confirmed/recovery alerts | 495 pre-merge coordinator tests; 23 final observer tests and 48-route integration; [release evidence](compute-observer-release-evidence.md) | Live 2026-10-17; 55 public checks passed after restart, two leases reconciled without findings, durable alerts and unchanged financial metrics verified, X card rendered |
| 3 | Aug 14 [workspaces](https://github.com/prismnetwork-tech/prism/pull/179) | Not included in this release | Separate parity branch | Not activated here |
| 4 | Aug 14 [canary](https://github.com/prismnetwork-tech/prism/pull/180) | Not included in this release | Separate parity branch | Not activated here |
| 5 | Aug 14–15 [SDKs](https://github.com/prismnetwork-tech/prism/pull/182) | Partial: JavaScript LangChain/LangGraph integration; native Python x402 client and official Microsoft Agent Framework tool/workflow. Other clients/frameworks remain separate | LangChain: 32 source + 32 installed-package tests. Python/Agent Framework: 71 source + 71 installed-wheel tests, canonical/signature vectors, shared Node/Python journal and budget, recovery and official registration; [Python release record](compute-agent-framework-release-evidence.md) | LangChain live 2026-10-18 with immutable v0.2.0 package and public page/card verified. Agent Framework funded acceptance passed: one 0.05-USDC payment finalized, pending recovery reused the original authorization, fresh-process replay made no network/signing/planner calls; package publication and public page/card still pending |
| 6 | Aug 15 [x402 seller](https://github.com/prismnetwork-tech/prism/pull/187) | Official x402 V2 Solana seller, durable reserve/replay state, real AgentKit wallet client, bounded provider operator | Focused gateway, official SDK, installed client and provider recovery tests; [release record](compute-x402-release-evidence.md) | Deployed 2026-10-18; funded AgentKit purchase finalized for 0.05 USDC with a verified native result and 0.03 USDC gross native Compute charge before marketplace fees. Production restart and original-authorization replay passed without another purchase; immutable client, public page and crawler-facing sharing card verified |
| 7 | Aug 15 [bounty](https://github.com/prismnetwork-tech/prism/pull/186) | Not included in this release | Separate parity branch | Not activated here |
| 8 | Aug 16 [tariffs](https://github.com/prismnetwork-tech/prism/pull/191) | Not included in this release | Separate parity branch | Not activated here |
| 9 | Aug 16 [discovery](https://github.com/prismnetwork-tech/prism/pull/192) | Not included in this release | Separate parity branch | Not activated here |
| 10 | Aug 16 [dashboard](https://github.com/prismnetwork-tech/prism/pull/193) | Not included in this release | Separate parity branch | Not activated here |
| 11 | Aug 17–24 [hardware trust](https://github.com/prismnetwork-tech/prism/commit/a066e1de717bd299a8ce942704f344bda8c31909) | Not included in this release | Separate parity branch | Not activated here |
| 12 | Aug 24 [batches](https://github.com/prismnetwork-tech/prism/commit/c5efc2823916a09b78008ec286b76029934136d0) | Not included in this release | Separate parity branch | Not activated here |
| 13 | Aug 24 [warm pool](https://github.com/prismnetwork-tech/prism/commit/4e52ebb42fd31f7f2f7b207ff7050b1a22d82ca6) | Not included in this release | Separate parity branch | Not activated here |
| 14 | Aug 25 [confidential inference](https://github.com/prismnetwork-tech/prism/commit/e7307a8e493eca00375f04c7d08ad565dc3e0008) and follow-up fixes | Not included in this release | Separate parity branch | Not activated here |
| 15 | Aug 29–30 [reproduction](https://github.com/prismnetwork-tech/prism/commit/740364facff6d07d1c9579881e39cbc014fcecc4) | Not included in this release | Separate parity branch | Not activated here |
| 16 | Aug 30 [host identity](https://github.com/prismnetwork-tech/prism/commit/20c2f58c96ab8b63946c8509b8f1e88cdcd290a4) | Not included in this release | Separate parity branch | Not activated here |
| 17 | Aug 30 [durable budgets](https://github.com/prismnetwork-tech/prism/commit/20c2f58c96ab8b63946c8509b8f1e88cdcd290a4) | Not included in this release | Separate parity branch | Not activated here |
| 18 | Sep 4 [billing](https://github.com/prismnetwork-tech/prism/pull/195), [readiness](https://github.com/prismnetwork-tech/prism/pull/196) | Not included in this release | Separate parity branch | Not activated here |
| 19 | Sep 16–26 [rails](https://github.com/prismnetwork-tech/prism/pull/199), [bridge](https://github.com/prismnetwork-tech/prism/pull/201) | Not included in this release | Separate parity branch | Not activated here |
| 20 | Sep 24–26 [decisions](https://github.com/prismnetwork-tech/prism/pull/200), [policy](https://github.com/prismnetwork-tech/prism/pull/201) | Not included in this release | Separate parity branch | Not activated here |

## 1. Historical GPU prices

`COVENANT_COMPUTE_PUBLIC_PRICE_HISTORY=1` enables the standalone `/prices`
page, `/prices/history` JSON API and an hourly read-only Vast market sampler.
The source is up to eight lowest whole-instance asks for each of eight named GPU
models, with 16 GB storage fixed in the query. Bandwidth and additional usage
charges are excluded. It is a limited market sample, not all provider supply or
proof that a listed machine meets Covenant admission policy.

Provider observations live in `provider-price-history.json` beside the existing
journal. Each snapshot is locked, bounded, fsynced and atomically replaced before
publication. Restart preserves observations and the hourly sampling boundary.
Failures retain prior data and report unavailable/stale rather than zero price;
an empty successful search is distinct. Retention is 366 days. Quotes that do not
change are sampled too, avoiding a mean biased toward frequently changing offers.

`LeaseAccess.source_price` is optional version-1 metadata. The Vast broker records
its selected offer's declared GPU model/count and whole-instance quote. The
coordinator stamps observation time and persists it in the existing journal.
Legacy messages and signing bytes remain valid. No old prices are backfilled.

`GET /prices/history?since_ms=…&until_ms=…` returns daily series, source/currency,
sample counts, observation bounds and explicit millionths per GPU-hour. Default
range is 30 days; maximum is 366 days. USD provider asks and gross USDC rental
charges stay separate. Multi-GPU rates use GPU-hours; settled rates are weighted
by metered duration. GPU identity is provider/operator-reported, not attested.

Settled series require a verified mainnet-USDC deployment pin in `price-history-identity.json`, and the same
pin durably recorded when access and payout are observed. Missing identity,
wrong chain/mint, old metadata, unknown-configuration periods, unpaid/refunded
jobs, probes and malformed receipts are excluded. Payout timestamps use the
observed payout time, not job completion or claimed block time. Signatures and
arithmetic are checked offline; transaction citations remain independently
checkable on chain and do not themselves prove finality.

All price surfaces are opt-in independently of the existing proof feed.
`/prices/proofs/:job_id` publishes only the exact qualified new lease records
represented in price history. The older full receipt feed stays private unless
a deployment separately enables it. The page
uses Covenant's published brand guide, separates sources/assets, shows gaps and
outages, and inserts provider labels as text. It exposes no buyer, prompt, SSH
endpoint or provider credentials. Range filtering precedes signature work;
blocking workers and a two-request concurrency limit keep public history off the
financial async path. Provider aggregation streams into daily counters instead
of duplicating a full year of offers.

## Release evidence and promotion

Local validation on 2026-10-17:

- 1,395 tests passed across coordinator, node, buyer and control (41 existing
  opt-in/ignored targets); 280 protocol tests passed separately.
- Eight page renderer tests passed, including hostile labels/links, separate
  sources/assets, missing days, exact small prices and outage handling.
- Both Compute documentation validators passed; affected coordinator/node Clippy
  (all targets, warnings denied) and formatting passed. Scoped price-proof and
  hostile-wire HTTP tests passed after the final privacy refinement.
- The repository-wide script gate has an inherited failure: 18 stale chain-CLI
  documentation line references. The script, cited CLI source and cited document
  are byte-identical to deployed `5b02333`; running the guard on that baseline
  produces the same errors. This is not a passing whole-repository CI claim.
- A real local coordinator queried the public provider with no credentials or
  paid operations: 53 offers across eight GPU models. Restart preserved the
  coordinator identity and exact persisted history without duplicate sampling.
- Identity, delayed payout, journal replay, unknown-network periods, source store
  corruption, interrupted writes and HTTP opt-in/privacy/range checks pass.

Production acceptance completed on 2026-10-17 at 20:11 UTC (22:11 CEST):

- [GPU price history](https://compute-api.opencovenant.org/prices) and
  [source JSON](https://compute-api.opencovenant.org/prices/history) are live.
- [PR #314](https://github.com/open-covenant/covenant/pull/314) merged as
  `6b6b74093362181e110513b91c3d6e9aa305c897`. The coordinator and L40S broker
  both run images from that revision. `/health` reports the exact revision.
- [Image workflow](https://github.com/open-covenant/covenant/actions/runs/39061576004)
  succeeded for both images. Anonymous GHCR manifest/config reads and all 15
  image-layer HEAD requests passed. OCI labels identify the same source revision
  and Linux/amd64 platform; mirror digests equal the deployed image digests.
- The new `compute-coordinator-public` and `compute-node-public` mirrors are
  public. Historical deployment packages retain their previous private visibility.
- The first durable provider sweep recorded 52 quotes across all eight tracked
  GPU models. The page/API reported available, fresh and persistence-healthy data.
- The coordinator identity remains
  `3Z27s9cCPNWYg7y86PhGwHpEMV4crrY5vAwTwzQNbjjh`. Its non-null settlement
  binding verifies Solana mainnet genesis and native USDC. The existing broader
  receipt feed still returns 404; nonexistent scoped price proofs return 404.
- A controlled production restart recovered the exact same eight price points,
  52 observations, sampling timestamp and persisted settlement identity. No
  duplicate sweep occurred. A brief restart outage was observed and recovered.
- Final checks found a healthy journal, all six registered operators live and
  the L40S lease operator matchable. The 66 completed, three failed and two
  refunded jobs were unchanged; there were no active jobs during promotion.
- Settled price samples remain zero. No paid rental or transaction was generated
  for this release. New qualifying leases will contribute their actual settled
  rates; neither old price history nor a funded production settlement test is
  claimed.

| Image mirror | Immutable tag | Verified digest |
|---|---|---|
| `ghcr.io/open-covenant/covenant/compute-coordinator-public` | `6b6b74093362181e110513b91c3d6e9aa305c897` | `sha256:36ccdca1f002acc81b65ceecba81522b74d9b3c8381f6d0ed3c7816fd25b9b4a` |
| `ghcr.io/open-covenant/covenant/compute-node-public` | `6b6b74093362181e110513b91c3d6e9aa305c897` | `sha256:cc130b3956a72d69dba7c75666816c9b9824062f65a734880b35d9dc81e5e381` |

The read-only release acceptance command passed after the restart:

```sh
node agent-os/scripts/accept-price-history-release.mjs \
  https://compute-api.opencovenant.org \
  6b6b74093362181e110513b91c3d6e9aa305c897 \
  3Z27s9cCPNWYg7y86PhGwHpEMV4crrY5vAwTwzQNbjjh
```

Disable `COVENANT_COMPUTE_PUBLIC_PRICE_HISTORY` to withdraw the public feature
while existing settlement and cleanup workers continue. Preserve the history
files and additive journal metadata. Do not replace the financial journal with
an older snapshot during rollback. No bounty, paid canary or later adoption
package is activated by this release.

## 2. Independent reconciliation and public status

The chain-first observer scans pinned Solana lease accounts independently of the
coordinator job list, compares retained journal and escrow evidence, and reports
aggregate findings through `/status` and `/metrics`. Empty scans, failed RPC,
stale reports, zero capacity and demonstrated discrepancies remain distinct.
Three observations confirm an alert, three complete clean scans confirm recovery;
a separate durable queue retains notification IDs across restart. Cloudflare Email Service email
or HTTPS webhook delivery is optional. No automatic financial repairs occur.

Configuration, interpretation, scope limitations and read-only acceptance are in
[compute-observer.md](compute-observer.md). The new endpoint excludes buyer/job
identities and secrets; reviewed incident narratives remain versioned source.
This package does not enable later parity features or change lease settlement.
Production evidence is recorded only after the deployed revision and alert path
are verified, separately from local test results.

Package 2 local validation (2026-10-17): 495 coordinator tests passed across all
targets (one existing opt-in test ignored), including 23 observer tests. Checks
cover wrong genesis/program/validator, old and future RPC data, chain orphans,
missing local settlement evidence, queue overflow, durable retry/restart and
confirmed recovery, Cloudflare recipient acceptance, privacy and hostile HTTP.
Coordinator all-target Clippy with warnings denied, formatting, and both Compute
documentation validators passed. A read-only public-mainnet RPC rehearsal found
two retained settled leases; the intentionally empty harness journal correctly
reported two settled orphans. This rehearsal did not compare production journal
contents, send transactions, or activate notification delivery.

On the final merge tree, all 23 observer tests and the combined 48-route
hostile-wire integration test passed. An isolated Cloudflare rehearsal preserved
a pending event across restart and received provider acceptance for firing and
recovery emails after three failed and three clean observations. Provider
acceptance does not establish inbox receipt.

Production was verified on 2026-10-17 at 22:54 UTC at the exact merged revision.
All 55 public page/API/card checks passed; the observer compared two retained
leases against the durable writable production journal with no findings. Email
alerts were configured with healthy persistence, zero active alerts and zero
pending deliveries. Financial and job metrics remained unchanged. The X composer
rendered the public card correctly. A controlled restart at 22:55 UTC preserved
the production alert state under `/data/mainnet`, advanced its observation time,
and left job/financial metrics and journal size unchanged. All 55 public checks
passed again at 22:57:09.672 UTC. Final website revision
`fa411e14027d531a94475cab797e866390847c5c` was live with the shared page layout,
refreshing status and a verified mobile viewport. Full identifiers and evidence
boundaries are in [compute-observer-release-evidence.md](compute-observer-release-evidence.md).

Release validation also repairs inherited documentation citations and aligns CI
with the signed Compute release's Rust 1.98.1 toolchain. Separate compatibility
repairs use fixed-size embedding chunks, checked reputation division, direct HTTP
authentication responses, and boxed fallback responses in the three SSE helpers.
They preserve embedding truncation, authentication status/body, stream ordering,
and public IPC/HTTP JSON; the shared `Response` enum and settlement code do not
change. All warning and test checks remain enabled. The release owner explicitly
approved promotion after focused validation without waiting for full-workspace
CI; those results are tracked separately and are not asserted green here. The
local macOS environment lacks the OpenSSL development prerequisite for the
workspace's Solana dependency tree.
