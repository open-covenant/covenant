# Model-specific inference pricing: evidence and release gates

Status: initial deployment completed on 19 October 2026; new admissions are
temporarily paused after tariff acceptance exposed a payment-verifier gap.
The repair, paid acceptance, and package publication remain pending. The initial
discovery and boot-probe evidence below do not establish a completed customer
purchase.

## Decision and customer behavior

Offer an explicit `pricing: "model_tariff_v1"` path for the already qualified
`claude-haiku-4.5` text model. Native settlement is 10,000 micro-USDC plus the
requested output cap at 7,500,000 micro-USDC per million; retail is 12,000 plus
9,000,000 per million. Each variable term rounds upward to a whole microunit.

The retail coefficients are a deliberate 20% markup policy. They are not rates
discovered from an upstream invoice. At the maximum 2,048-token cap, retail is
0.030432 USDC, 39.136% below the existing 0.05 USDC flat offer. The fixed quote
settles in full even when the generated answer is shorter.

Omitting the new pricing field retains the existing 50,000 retail / 30,000 native
micro-USDC flat path. Existing client examples and persisted purchases must remain
valid. No change to the provider model, route, input-byte limit, output limit,
spending ceilings, or full-input reservation policy is included.

The gateway limits a fresh x402 payment quote to at most 75 seconds from its
prepared native envelope's issue time: the 120-second native freshness window
minus a 45-second delivery margin. A nearer tariff expiry can shorten it further.
Admission checks the remaining native deadline and tariff validity before any
new paid execution. Recovery uses the stored signed envelope and transaction.

Public tariff configuration also rejects values above JavaScript's maximum safe
integer (`9007199254740991`), including revision, base/rate, validity timestamps,
and maximum computed price. This prevents a valid Rust `u64` value from being
rounded when the supported clients verify the public JSON quote.

## Published references and provider observations

Read on 19 October 2026:

- [OpenRouter's exact Haiku 4.5 page](https://openrouter.ai/anthropic/claude-haiku-4.5)
  lists `anthropic/claude-haiku-4.5`, a 200,000-token context window, and standard
  input/output prices of 1 / 5 USD per million tokens. This is a published
  upstream reference, not evidence of a particular UsePod account debit.
- [UsePod's proxy reference](https://docs.usepod.ai/api/proxy/) documents the
  token-scoped `GET /v1/models` catalog and input/output price-ceiling headers in
  USDC microunits per million tokens.
- [UsePod's routing reference](https://docs.usepod.ai/marketplace/routing/)
  documents `X-Pod-Providers: openrouter` as a hard provider constraint and
  `centralized-only` routing. The catalog GET returned the exact namespaced Haiku
  model with `owned_by: openrouter`; a listing does not prove inference acceptance.
- [UsePod's spending controls](https://docs.usepod.ai/using/spend-controls/)
  describe refusal when no eligible provider fits both price ceilings. Covenant's
  existing bridge retains ceilings of 1,500,000 input and 7,500,000 output
  micro-USDC per million.

Six successful planner calls from the existing LangGraph and Microsoft Agent
Framework acceptance work reported the following usage and costs in their stored
upstream responses. These are the existing OpenRouter-pinned route's measurements;
they are not new pricing-path acceptance calls or independently audited invoices.

| Input tokens | Output tokens | Provider-reported USDC cost | Conservative accounted micro-USDC |
|---:|---:|---:|---:|
| 899 | 280 | 0.002299 | 3,449 |
| 854 | 231 | 0.002009 | 3,014 |
| 899 | 280 | 0.002299 | 3,449 |
| 875 | 238 | 0.002065 | 3,098 |
| 897 | 315 | 0.002472 | 3,709 |
| 750 | 181 | 0.001655 | 2,483 |

Each reported cost matches 1 micro-USDC per input token plus 5 per output token.
Conservative accounting uses the higher configured ceilings and independently
rounds input and output upward. The underlying local evidence is the existing
private `x402-launch/provider-qualification/calls.sqlite` ledger, with call groups
`langgraph` and `agent-framework`; response text and credentials are not published.
The older single qualification result is excluded from this price comparison
because it predates the current pinned response-identity path.

Four existing completed customer purchases bind `claude-haiku-4.5` to signed
provider metadata reporting `anthropic/claude-haiku-4.5`, `openrouter`, and
`centralized`. Their requests all used a 2,048-token output cap, with message-array
sizes of 574, 925, 1,451, and 1,342 bytes. Those customer responses do not expose
upstream usage. They establish previous flat-path payment and signed identity
behavior, not measured economics for the entire 4,096-byte input allowance.

## Limits of the economic argument

The 10,000-micro native base exceeds the measured input component of these small
samples and allows a commercial contribution toward input and request overhead.
At the configured input ceiling it corresponds to about 6,666 input tokens. A
4,096-byte message limit plus protocol framing appears compatible with that
allowance, but no tokenizer qualification or enforced upstream token-count limit
has established it. The tariff therefore does not guarantee a positive margin
for every accepted provider report.

Keep the 200,000-token provider input reservation. For a requested output cap N,
the reservation remains `300000 + ceil(N × 7500000 / 1000000)` micro-USDC,
or 315,360 at N = 2,048. The reservation protects the spending allocation against
uncertain outcomes; it is not an estimate of normal request cost.

No claim of net profitability is made. Prior customer settlement evidence records
10,001 lamports in Solana fees per transaction; hosting, failures, and other
operational costs have not been allocated to the proposed price. The samples do
not establish rates for other models, longer inputs, tools, images, or streaming.

## Historical production baseline

At approximately 00:40 UTC on 19 October 2026, authenticated Render GETs showed:

| Service | Live deployment | Immutable image digest | Other state |
|---|---|---|---|
| Inference/x402 `srv-dba12mu1egvs738v5no0` | `dep-dbalu6k9v7es7391j2gg` | `sha256:d95c659e209829640c67ff7930fcfd66b2bd330c5e8c50f2542e6e155114a1ac` | 50 environment keys; watchdog enabled; no pending deploy |
| Coordinator `srv-d9agfv7aqgkc739jpcn0` | `dep-dbam8rs9v7es7392pb0g` | `sha256:046b7ce0bcea4b9814452908201a699b6b0ef0a828eec6a57d06b53a2c9682f8` | 78 environment keys; canary mode `once`; no pending deploy |

The public inference `/health` then reported core revision
`e52e2a790e1dbabf9a7e5018cdca99e82cdcea9e`; the newer immutable image includes the
watchdog overlay. Image identity and the core health revision are different
evidence. Reusing the earlier x402 image would discard the overlay. At this
baseline `/v1/models` returned 404, so the new gateway catalog was not yet live.

Both services used one Frankfurt image-runtime instance and an existing `/data`
disk. This historical snapshot preceded the subsequent rental-canary release;
the activation evidence below records the later deployment.

Existing private rollout helpers use authenticated Render GET snapshots, exact
environment-digest checks, an immutable image digest, and a durable intent before
one deployment POST. Ambiguous POST outcomes are reconciled through GETs, never
blindly submitted again. The historical research reads made no Render mutations.

## Historical acceptance headroom

The existing customer journal held six 50,000-micro reservations: four completed
and two uncertain attempts retained. Against its configured 500,000 total ceiling,
that leaves 200,000 micro-USDC of historical policy headroom. The wallet held
300,000 micro-USDC at finalized Solana slot 458637756, read at 00:42:25 UTC.

The separate local planner provider ledger accounted for 619,765 micro-USDC,
including 600,480 in previously abandoned full losses, with no unresolved calls.
Its 1,000,000 configured ceiling leaves 380,235 of local historical headroom.
The production provider journal is different: these local totals must not be used
as its current balance or remaining allowance.

An authenticated, read-only production Render-shell inspection at
00:50:54.581 UTC recorded 10 provider calls and 335,350 micro-USDC of lifetime
accounting. Nine completed calls account for 19,990; one unknown call retains its
entire 315,360 reservation under an existing abandoned-full-loss record. There
are zero unresolved calls: an abandoned loss is retained spending accounting,
not an outstanding dispatch. Rolling-day accounting was 335,292. Both configured
ceilings remained 1,000,000, giving 664,650 lifetime and 664,708 rolling-day
policy headroom at that observation. These are accounting limits, not a provider
account-balance statement or a new spending authorization.

The same read recorded gateway journal version 2, five entries (four complete,
one failed), and the existing 3,000,000-micro reserve. Its journal SHA-256 was
`dcc783aa2dcd0a3985f6ecec1c82fc4babe7c38ed6f55a1297a5f831970c1bed`.
The private observation is retained as
`model-pricing-launch/production-totals-20261019T005054Z.json`. It dispatched no
provider calls and changed no remote state.

Available balance and historical caps do not themselves authorize new acceptance
spending. Confirm the task's funded allowance before signing or dispatching new
paid requests. Read the production provider ledger and reconcile every unresolved
payment/provider outcome first. Never recycle the two uncertain customer IDs,
reset a ledger, or raise a budget as part of tariff activation.

## Deployed source and activation

[PR #353](https://github.com/open-covenant/covenant/pull/353) was reviewed at
`54a1b2dac8bcfade63da3fc08816596cb606edd6` and merged at
`367199ad3bfa85c2a77ce2d06b391350d16c7e22`. The merge includes the separately
accepted rental-canary documentation from PR #356. Its runtime, deployment,
service, SDK, and workflow files match the reviewed source; the complete trees
differ only in `docs/compute-prism-parity.md`. Post-merge CI passed.

| Service | Source revision | Immutable image | Render deployment and live time (UTC) |
|---|---|---|---|
| Coordinator | `367199ad3bfa85c2a77ce2d06b391350d16c7e22` | `ghcr.io/open-covenant/covenant/compute-coordinator-public@sha256:1f5e084b252f15be1910b261f3e34c885726956d0f4084cae3fbee01c7b7cc4e` | `dep-dbb00hmq1p3s73db02e0`; 2026-10-19T11:35:11.639702Z |
| Inference/x402 | `54a1b2dac8bcfade63da3fc08816596cb606edd6` | `ghcr.io/open-covenant/covenant/compute-node-public@sha256:030483a9b1cbcb2d19dfba6084802856494ae4d4e741d95d583a7482fe174086` | `dep-dbb01bvbtjjs738rbmd0`; 2026-10-19T11:36:45.914467Z |

Authenticated Render readback confirmed both exact images were live. Anonymous
health reads confirmed the coordinator revision at 11:35:22 UTC and the inference
revision with admissions enabled at 11:37:31 UTC. The inference image includes
the existing watchdog. The coordinator retained scheduled rental checks, with
no active or unresolved canary entry at the post-deployment observation.

At 11:37:31 UTC, the public
[`/v1/models`](https://compute-inference.opencovenant.org/v1/models) catalog
returned HTTP 200 with `Cache-Control: no-store`, the Haiku model, and its retail
tariff. Public
[`/federation/capacity`](https://compute-api.opencovenant.org/federation/capacity)
advertised the matching native tariff alongside the existing 30,000-micro flat
ask. Both tariffs use schema 1, revision 1, and a 2,048-token maximum:

| Tariff ID | Base micro-USDC | Output-cap rate per million | Valid from (inclusive, UTC) | Valid until (exclusive, UTC) |
|---|---:|---:|---|---|
| `covenant-haiku-4.5-native` | 10,000 | 7,500,000 | 2026-10-19T11:35:00Z | 2026-11-18T11:35:00Z |
| `covenant-haiku-4.5-retail` | 12,000 | 9,000,000 | 2026-10-19T11:35:00Z | 2026-11-18T11:35:00Z |

The startup qualification made one provider call, completed at
2026-10-19T11:36:46.890Z, with 13 input and 5 output tokens. It added **58
micro-USDC** of conservative provider accounting. It was a boot probe, not a
tariff customer purchase. The post-activation provider ledger held 11 calls,
335,408 micro-USDC of lifetime accounting, and zero unresolved calls. Both
provider ceilings remained 1,000,000 micro-USDC; the prior 315,360-micro abandoned
loss remained counted.

The gateway journal migrated from version 2 to 3 and retained the observed
aggregate of five entries (four complete, one failed) and the 3,000,000-micro
reserve. Exact historical entry hashes were first captured after migration;
aggregate agreement does not prove every old entry was semantically unchanged.
At the separate coordinator readback, the gateway buyer still had 2,880,000
micro-USDC available and zero unsettled jobs. These are timestamped state
observations, not an additional spending authorization.

Sanitized private readbacks are retained in
`model-pricing-launch/public-deployment-evidence.json`,
`coordinator-runtime-after-deployment.json`, and
`inference-runtime-after-activation.json`. The public readback SHA-256 is
`8ead271e034829c73911b4eae1d6ecb9ff73d9fbecd97071c6dcd0227c59c3c0`.

## Local validation and package candidates

The release work validated the working tree locally on 19 October 2026. The
client suites passed with the real local Solana facilitator bridge enabled:

| Check | Result | Scope |
|---|---|---|
| Native protocol, buyer, coordinator, and node library checks | 1,244 passed, 0 failed, 20 ignored | Buyer 250/1 ignored; coordinator 369/0; node 322/19; protocol 303/0 |
| Gateway unit suite | 19 passed, 0 failed, 0 ignored | Local quote, payment-state, gateway behavior, freshness margin, and public safe-integer bounds |
| Actual gateway binary startup/recovery test | 1 passed, 0 failed, 0 ignored | Malformed-tariff recovery-only startup, version-2 journal migration, and settlement using the same transaction through a local mock payment service |
| Payment facilitator repair | 11 passed, 0 skipped | Independent tariff/legacy admission, exact transfer and sponsorship bounds, retained sealed-payment recovery, and malformed configuration |
| x402 runtime and smoke helpers | 16 passed | Local runtime configuration and smoke-check behavior |
| JavaScript x402 client | 56 passed, 0 skipped | Local fixtures, signature/quote checks, payment journal and replay; includes the verifier-repair regression below |
| Python x402 client | 90 passed | Local fixtures, signature/quote checks, payment journal and replay; includes the verifier-repair regression below |

Commands used for these suites (the Rust invocations reuse the existing local
build cache through `CARGO_TARGET_DIR`):

```sh
# From the repository root.
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test --locked --manifest-path agent-os/Cargo.toml \
  -p covenant-compute-protocol -p covenant-compute-buyer \
  -p covenant-compute-coordinator -p covenant-compute-node --lib --quiet

# From agent-os; CARGO_TARGET_DIR may reuse the existing local build cache.
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test -p covenant-compute-gateway

# From the repository root.
CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test --locked --manifest-path agent-os/Cargo.toml \
  -p covenant-compute-gateway --test startup_recovery -- --nocapture

# From the repository root, using Node 24.
COVENANT_X402_BRIDGE_PATH="$PWD/services/compute-x402-facilitator/bridge.mjs" \
  node --test sdk/compute-x402-client/test/*.test.mjs

# From sdk/compute-x402-python, using the existing qualified Python environment.
PYTHONPATH="$PWD/src" python -m pytest -q
```

The primary suite results were captured in the task's execution output. Ignored
native tests were not exercised. After moving the unchanged 120-second native
freshness limit into a shared protocol constant, 15 protocol-envelope tests and
one node stale-admission test passed again; these repeat existing tests and are
not added to the 1,244 count. The binary recovery test uses an isolated local
journal and loopback payment fixtures; it is not a production restart or a chain
transaction.

`cargo fmt --all --check`, `git diff --check`, and the complete
`bash agent-os/scripts/validate.sh --scripts` gate passed. The scripts gate ran
223 checks, including the generated README metrics, after updating only its
generated metrics block with `node agent-os/scripts/metrics.mjs --write`.
Its local output is retained in
`/tmp/covenant-model-pricing-validation-20261019/scripts-pricing-ops.log`.

The JavaScript package was built with `npm pack --pack-destination dist --json`;
Python used `python -m build` to produce an isolated wheel and source distribution.
SHA-256 values were independently read back from these local package files:

| Local package candidate | SHA-256 |
|---|---|
| `sdk/compute-x402-client/dist/covenant-compute-x402-client-0.3.0.tgz` | `1ff1a48f7b81032cccfe7c70f658c6a9b6ca41d972f3105735af4df42ca31abb` |
| `sdk/compute-x402-python/dist/covenant_compute_x402-0.2.0-py3-none-any.whl` | `e2d12dade31a531064eac83bb99ad5961b764bbf2bca8e568bba314e3a7ee15d` |
| `sdk/compute-x402-python/dist/covenant_compute_x402-0.2.0.tar.gz` | `ecca00f8a85ab16e58b7e55cbbbaee355a94a4051cd4b6549007826cab091981` |

The extracted JavaScript archive and an installed Python wheel each passed a
local artifact-import smoke check: opt-in variable quote, a pending response,
client/process restart, and exact saved completion replay, with one quote and
one signature. The 1,200-micro fixture amount is synthetic test data, not a live
tariff. Rust-generated signed fixtures exercise both client verifiers.

The native buyer, node, and lease-signer manifests are aligned at `0.4.0`.
The archive README references only `compute-v0.4.0`, and matching notes exist at
`deploy/compute-release/notes/compute-v0.4.0.md`, satisfying the packaging metadata
preflight. All four platform build jobs passed for the reviewed PR source;
tag-triggered signing and publication remain pending. No package was published
by these checks.

## Paid acceptance and publication gates

The first installed JavaScript acceptance attempt at 11:39:26 UTC received a
14,304-micro quote for a 256-token cap, then stopped before signing. The original
child outcome was not captured, so the cause of that stop is not established.
It produced no payment authorization or paid gateway submission;
the provider ledger was unchanged by that attempt. Its reserved client row and
one-attempt record remain retained. This is an incomplete acceptance attempt,
not a successful purchase or an uncertain on-chain payment.
Read-only local state is retained in `js-attempt-readonly-state.json` and
`acceptance-runners/failure-diagnostics/sqlite-diagnosis.json` under the private
release evidence directory.

A second attempt stopped before a payment submission: the private JavaScript
harness erased a byte array retained by its signing key, producing an invalid
signature. The unchanged installed SDK rejected that signature. The harness
was corrected separately; disposable-key checks verified both exact wallet
adapters with the installed payment validators, without live keys or network
calls. Both earlier attempt records and unpaid reservations remain retained.

The corrected JavaScript harness then made one authorization-bearing request
for 14,304 micro-USDC. Production returned HTTP 402 with
`payment rejected: authorization rejected by payment verifier`. The deployed
facilitator still admitted only its independently configured 50,000-micro flat
price, despite the gateway advertising valid tariff quotes. This exposed a gap
in the earlier local bridge test, which had copied its configured price from
the test payment. The saved authorization and reservation remain retained;
this is not a successful customer purchase and must not be blindly reposted.
At 11:54:08 UTC, a read-only finalized Solana check at slot 458823836 still
reported the customer's 300,000-micro-USDC balance and no finalized account
activity since that attempt. The observation is retained in
`signing-diagnostics/v3-readonly-balance-1792410848995.json`.

At 2026-10-19T11:56:23.954511Z, public readback confirmed the inference service
remained healthy at revision `54a1b2dac8bcfade63da3fc08816596cb606edd6` with
`admissions_enabled: false` and an empty model catalog. This temporary pause
protects new purchases while the facilitator repair is qualified. The
coordinator deployment is unchanged. Evidence is retained in
`repair-admissions-paused.json` and
`acceptance-runners-v3/javascript/paid-response.json`.

The prepared repair loads the same paired tariff configuration into the
facilitator and admits exact integer-cap retail prices alongside the fixed
legacy price. It refuses nonmember amounts before sponsor signing and preserves
recovery of sealed original authorizations after expiry, rotation, or invalid
optional pricing configuration. JavaScript and Python regressions now send
14,304-, 30,432-, and 50,000-micro authorizations through one real facilitator
with an independently configured policy. Nonmember prices, changed accepted
terms, and mismatched transfers are rejected before sponsor signing; the tests
perform no broadcast. The JavaScript regression fails against the original
deployed bridge. These local checks do not establish deployment of the repair.

| Installed client | Requested output cap | Expected retail / native micro-USDC | Current evidence |
|---|---:|---:|---|
| JavaScript 0.3.0 | 256 | 14,304 / 11,920 | Two unpaid harness attempts retained; corrected harness reached verifier rejection; acceptance pending repair |
| Python 0.2.0 | 2,048 | 30,432 / 25,360 | Paid acceptance pending |

Before declaring the release accepted, record the completed tariff receipts,
finalized customer transfers and native settlement, actual provider accounting,
and exact saved-completion replay without a new purchase. Preserve the original
customer journal, its uncertain entries, and all spending ceilings. Complete
unpaid legacy/invalid-request controls separately from paid provider acceptance.
First qualify and deploy the repaired inference image, reconcile retained
attempts without reposting them, and verify admissions and actual pricing again.

Immutable SDK assets and the signed `compute-v0.4.0` native release remain
unpublished. Public page/card publication is a separate step. No paid tariff
completion or increased budget is claimed in this record.
