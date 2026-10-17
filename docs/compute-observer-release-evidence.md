# Compute observer release evidence

Status: live; production page/API, controlled restart and sharing-card acceptance
completed on 2026-10-17 at 22:57 UTC.

Release: [PR #323](https://github.com/open-covenant/covenant/pull/323), merged as
`28cff590ca5764dfddcc07d729f1e92ed5e8761e`.
The tested merge tree was `d4bb88565f187aec60198b1cd9191e27bc48cae6`, incorporating
deployment base `b0d5910b60be2a4c370f356fdec6dc221c7a9378`. This record concerns
Prism adoption package 2; existing deployment-base changes remain separate work.

## Implemented scope

- An alert-only observer starts with scoped Solana lease accounts, then compares
  local journal and settlement evidence. It checks genesis, executable program,
  account ownership/layout, public deployment pins, RPC freshness and rollup
  identity. Scans and RPC responses are bounded.
- Findings distinguish missing local records, overdue or partial settlement,
  contradictory terminal states, missing payout evidence and charge mismatches.
  RPC blindness, stale data, empty chain scope and empty capacity are distinct.
- Versioned `/status` and `/metrics` expose aggregate evidence and freshness.
  Public status omits job/buyer identities, RPC credentials and alert recipients.
  Incident narratives must be reviewed and committed; the initial list is empty.
- Alerts require three confirming observations; recovery requires three complete
  clean observations. A separate durable queue retains event IDs across restart.
  Cloudflare Email Service and HTTPS webhook transports retain uncertain sends
  for retry. Delivery is at least once; provider deduplication is not promised.
- Package 2 neither signs transactions nor changes settlement, fees or cleanup.
  It does not infer missing chain leases from local jobs because the current
  sidecar permits off-chain fallback without a durable successful-open marker.

## Validation already completed

| Evidence | Result and boundary |
|---|---|
| Coordinator suite before the final deployment-base merge | 495 passed, one existing opt-in test ignored; includes 23 observer tests |
| Observer tests on the final merge tree | 23 passed: pinned identity, stale/future data, orphan and settlement controls, privacy, persistence, retry and recovery |
| Combined router integration | Hostile-wire test passed for all 48 routes, including `/status` and upstream Kani evidence; no money moved |
| Merge checks | Formatting, both Compute documentation validators and both conflicted citation validators passed |
| Inherited validation repairs before the final merge | 757 daemon, 66 memory and 18 HTTP gateway tests passed; 223 documentation guards and affected-package Clippy passed |

The inherited repairs preserve HTTP/IPC payloads, authentication responses,
stream order, reputation arithmetic and embedding truncation. They are separate
from observer behavior. Full-workspace CI is not asserted green; the release
owner instructed promotion after focused checks without waiting for CI. The
local full-workspace lint run also lacks the OpenSSL development prerequisite.

A read-only mainnet rehearsal found two retained settled lease accounts. An
intentionally empty local journal correctly produced two settled-orphan findings.
That rehearsal did not compare the production journal or submit transactions.

## Isolated Cloudflare acceptance

On 2026-10-17, 22:14:33–22:15:15 UTC, the actual Cloudflare adapter accepted a
firing email and a recovery email to the existing verified mailbox used by
`alert@opencovenant.org` routing in an isolated rehearsal. This proves provider
acceptance, not inbox delivery.

Three failed observations produced one durable `rpc_unavailable` event. Reopening
the state file preserved the same pending event ID. Three complete live-mainnet
scans over an intentionally empty scope confirmed recovery. Final state:
`active_count=0`, `pending_delivery_count=0`, `persistence_healthy=true`, with two
delivery acknowledgements. The state file used mode `0600` and was removed after
the test. No production state, financial worker or transaction was used.

Acceptance source: observer commit `7b961af51973e06dab892b28b14c0afb73929b82`,
worktree head `ddcd453034c6c65393c39f9cad5da705d2ebbed1`.
Recorded JSON SHA-256:
`d765b4f1aa6b24e113620602638c038c5e5e775f41ac4207594adb0e2a1d92e5`.

## Production acceptance

The coordinator deployment `dep-db9vocom7kps73etseu0` began on 2026-10-17 at
22:52:35 UTC and completed in 34.7 seconds. Its reported revision is exactly
`28cff590ca5764dfddcc07d729f1e92ed5e8761e`. The immutable image digest is
`sha256:f7f7492d0ae9e5364d35aaee4046c0481060b002d195d64a711735de68dcb781`;
anonymous downloads of all nine image layers were verified before promotion.

At 22:54:14 UTC, 55 public acceptance checks passed for
[Compute status](https://opencovenant.org/compute-status), its same-origin data
proxy and the [coordinator status API](https://compute-api.opencovenant.org/status).
Checks covered fresh schema-valid snapshots, exact revision/scope agreement,
durable writable journal, healthy alert persistence, configured email transport,
no-store responses, rejected unsupported query parameters, canonical URL and the
anonymously available 1200 × 630 PNG sharing image. The actual X composer also
rendered the card correctly; `compute-status-x-preview.png` records that manual
check.

The pre-restart snapshot, generated at 22:54:23.492 UTC, records:

- Schema 1, `alert_only`, complete scan at 22:54:08.282 UTC, L1 slot 458209342.
- Two retained leases compared with the production journal, `healthy`,
  `leases_observed`, and no findings. This is scoped reconciliation evidence,
  not a solvency, uptime or future-settlement guarantee.
- Six registered and matchable operators; no reviewed incident entries.
- Durable writable journal; healthy email alert persistence, zero active alerts
  and zero pending deliveries. No production alert delivery was claimed.
- All six chain-scope pins matched the verified production configuration:
  genesis, program, mint, lease coordinator, funding renter and rollup validator.

Production financial and job metrics were unchanged from the saved pre-promotion
snapshot. The journal was 4,328,622 bytes. No paid job or financial transaction was
created for this release acceptance.

The website first went live at `44fb8e28fd23b13161560760a11fc70cb99435f9`, deployment
`dep-db9vlfrm8hqs73aue8dg`. Final website revision
`fa411e14027d531a94475cab797e866390847c5c`, deployment
`dep-db9vn661egvs73d7mhg0`, includes that change and was confirmed live. At a
390-pixel viewport, document and body widths were both 390 pixels. Public data
refreshed from 22:56:19 to 22:56:50 UTC. The X composer draft used to verify the
card was explicitly discarded and its tab closed; no post was submitted.
The expanded deployment identity matched all public pins, and the browser
console showed no hydration, React or uncaught errors during acceptance.

Render recorded the controlled coordinator restart at 22:55 UTC. Instance suffix
changed from `rlrb5` to `d7znh`. The production alert state at
`/data/mainnet/observer-alert-state.json` remained present, 176 bytes, mode `0600`,
schema 1, with deployment binding
`5HhveeqNCj3kZysRHtWzk4YS8xyQoHW2LgAwpjaap82L`, empty tracks and empty pending
queue. Its last observation advanced to `1792277809900`. The state hash changed
as observation timestamps advanced; byte identity was neither expected nor
claimed. The financial journal remained 4,328,622 bytes, and all saved job and
financial metrics remained identical after restart.

The final post-restart public acceptance at 22:57:09.672 UTC passed all 55 checks
with zero failures. The observer again reported a complete healthy comparison of
two retained leases, a durable writable journal, and healthy email alert
persistence with zero active or pending alerts.

Evidence SHA-256 values:

- Public acceptance JSON: `25e58274f1c32f5fa32f9ba192996e638be145f147eeff556125bbb797b7e5dd`.
- Pre-restart status JSON: `6c9c5c55e8b76f519a34eacf6650bfdad08a4cd08beb6687becc4c5c5371609b`.
- Pre-promotion metrics: `04ba2d608a5029b3246fa58b0ef6903a29b6f4d1d5d3f372ca1bcb1979095ce5`.
- Final post-restart acceptance JSON: `39160b81734a158a1d0537c5c722f1e7f2c04654433bd356f243c0bd80dfc6da`.
- Post-restart status JSON: `4741b47b25155a63b7d232eb4aef1b88c19eca82b981fcd3b66d5a3552d5be55`.
- Post-restart metrics: `07339cc3c40d1cd586769e0de4566909cdaf6be50ae2576f0af3a7840187ecc1`.
- Sanitized `compute-status-release/release-verification.json` manifest, including
  manual X and restart evidence: `1f6ce8ca6e8ac1c501e20e56ccbef42b99cf3ce4d56cce06c49450e927717b8b`.

Email inbox delivery remains unverified; the two firing/recovery acknowledgements
above belong to the isolated Cloudflare provider rehearsal.
