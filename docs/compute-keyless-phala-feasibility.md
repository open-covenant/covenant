# Phala without managed KMS — isolated test, 2026-10-20

This is the historical record of the first direct-ingress experiment. Later
outbound work is recorded in the [ephemeral Phala acceptance report](compute-ephemeral-phala-acceptance.md);
it does not change this test's refused ingress or failed disk reuse.

**Result: deployment feasibility observed; independent hardware and buyer-channel
acceptance not established. Production remains disabled.**

One disposable Phala Cloud CVM accepted an exact `key_provider: "none"`
configuration, reached provider state `running`, and logged temporary guest key
generation with provider identity `none`. Direct TCP access was refused for the
workspace. A subsequent configuration update restarted the CVM, which generated
new temporary keys but could not open its existing encrypted disk. It failed
before the added serial quote exporter ran. No genuine quote or SSH connection
was independently verified during this test.

The test CVM was deleted. The provider returned DELETE 204, then exact-UUID GET
404; an authenticated workspace listing also omitted it. The earlier diagnostic
CVM was left unchanged and remained listed. This test used a separate identity,
disk, SSH key and private evidence directory, outside the production lease journal.

## Scope and immutable identities

| Item | Value |
| --- | --- |
| New CVM UUID | `15522af2-3051-4f77-be3c-30abf3663caf` |
| Application ID | `4d5c803167e0c1a2aeeef93bc282d16105df0db5` |
| Diagnostic job | `162a65df-c44e-452d-aad8-6d4589833023` |
| Provider placement | prod5, legacy Teepod 26 / node 5 |
| Size | `tdx.small`, 1 vCPU, 2 GiB RAM, 20-GB disk |
| OS | production `dstack-0.6.1` |
| OS image hash | `ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6` |
| Guest container digest | `sha256:42893bf85bc07eb1bd3e9e5a48fe3ab3cd36b931d548080314691f7c7134cc7a` |
| Guest image source | `232bf28755387a4cbf803a53b02fc0880e0eb7c7` |
| Initial exact workload hash | `ce8672ac1ca901191b1f9f2d87c84cdce6976195371d588a87ed95db8dc577e7` |
| Serial-quote update workload hash | `a1c7e0b6d4d8784335b6c668c562ed9c1f0005b8d8591620512c277b739560cf` |
| Test application expiry | `2026-10-20T20:53:40.980Z` |

The final initial wrapper explicitly disabled KMS, local key provision, gateway
and tproxy; used an empty feature/environment allowlist; and retained
`"swap_size":"0"`. No encrypted environment or customer credentials entered
the VM. The buyer SSH private key remained local. The pinned image manifest and
configuration were freshly checked through anonymous GHCR reads.

The first preparation response omitted the empty `key_provider_id` field.
Its hash mismatch prevented creation. A second local candidate omitted that
field explicitly; all other fields remained identical, and the second provider
response matched the independently serialized bytes exactly. dstack defaults an
omitted provider ID to an empty byte vector. Both preparation responses were
retained; only one create request was made. Returned KMS metadata was not treated
as evidence of runtime key use or independence.

The live advertised rate was $0.058000/hour compute plus $0.002780/hour disk,
total $0.060780/hour. Creation allowance was one CVM with a one-hour deadline.
A scheduled-delete request was acknowledged, but its response did not expose
the stored deadline; cleanup was therefore performed and verified explicitly.
The final provider invoice was not checked.

## Live results

All times below are UTC.

| Event | Time / result |
| --- | --- |
| One create acknowledged | `19:55:35.994`, HTTP 200 |
| Initial create operation completed | `19:56:08.218` |
| Direct TCP allocation | HTTP 403: `Network configuration is not enabled for this workspace` |
| Control-plane attestation read | HTTP 200, but certificates and TCB info were null |
| Exact serial-quote update accepted | `20:01:30.050`, HTTP 202; no retry |
| Updated boot failed | `20:02:19.898`, encrypted-disk open failure |
| Delete acknowledged | `20:03:30.948`, HTTP 204 |
| Exact UUID absent | `20:03:43.596`, HTTP 404, plus workspace-list absence |

Initial serial output recorded temporary app-key generation,
`KeyProviderInfo { name: "none", id: "" }`, formatting an encrypted data disk,
no encrypted environment, disabled gateway, and both diagnostic containers
starting. This is provider-delivered log evidence, not a signed attestation.
The wrapper requested zero swap, but this test did not independently verify its
live kernel or cgroup swap state. The earlier managed-KMS memory diagnostic is
separate evidence and must not be transferred to this profile.

The provider's normal gateway cannot simply be retained: non-KMS app keys have
an empty gateway identity, and the reviewed guest rejects gateway setup without
one. See [key construction](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/main.rs#L1229-L1237)
and [gateway check](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L700-L707).
The workspace's direct-network permission refusal was respected.

The serial-export update was intended to obtain a nonce-bound raw TDX quote,
including an RTMR-bound disk-marker/swap observation. The exact updated hash
matched before application. On reboot the guest instead detected the existing
LUKS header, generated temporary keys again, and reported
`Failed to open encrypted data disk`. The provider independently recorded the
compose-update operation as failed. The exporter never ran, so no quote,
marker-erasure assertion or successful restart is claimed.

This matches the reviewed source: the `none` path generates fresh app/disk keys,
while filesystem initialization detects an existing LUKS header and attempts to
open that disk. The variable named `disk_reusable` governs instance-seed handling;
it does not establish successful disk reformatting on every restart.
See [temporary key generation](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L2481-L2491),
[disk detection](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L2577-L2591)
and [instance-seed handling](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L2942-L2950).

## Qualification boundary and next design

Production source, trust gates and the earlier diagnostic VM were unchanged.
The private direct-TCP probe passed 14 offline tests. A private no-KMS verifier
passed 15 profile/transport tests; a separate boot-only verifier passed three
boundary tests. These prepared tools produced no accepted live result.

The next design identified by this test was a fresh VM and empty disk for every
lease or recovery, rather than an update/restart of the existing encrypted disk.
It also needed an outbound connection whose guest key is independently bound to attestation,
or supported direct ingress. Outbound image downloads worked in this test;
an outbound buyer relay had not been implemented or tested at this stage.
The subsequent implementation and its separate live evidence are recorded in
the [outbound acceptance report](compute-ephemeral-phala-acceptance.md). A relay alone does
not establish workload integrity, secret custody, or lease authorization.

At the end of this test, the remaining gates were to collect and verify a fresh
quote over that usable channel, qualify the complete runtime and
ephemeral-storage design, demonstrate guest-key possession, and finish paid
provisioning, expiry, settlement and deletion tests. The factual Phala hosting
association was established; provider-independent secure execution remained
unqualified. The [managed-KMS rejection](../deploy/compute-attested-cpu/kms-policy-review.md)
and [production acceptance gates](compute-hardware-acceptance.md) remain in force.

## Privately retained evidence hashes

Raw API responses, commit tokens and logs remain private. Selected SHA-256 hashes:

| Artifact | SHA-256 |
| --- | --- |
| Exact preparation response | `d1b66db5d8584b0aa8bb99381e09717b16a51bf34ba8b7205678203a3fc9820a` |
| Initial serial log | `9eaa39db5c3746ded5149efa06a6504795bd1f6ef8343917e9a4e78824bf5ec6` |
| Direct-network refusal | `c9c398fb0a70fd88b518aa4b8e979137f9161e6d576b27d3a5c5bc44c0186c6e` |
| Failed restart serial log | `e58c7c760181645b4e5112d0eb90fc8785f4f3673f1d8d43a1b43a5359afb333` |
| Final provider operations | `1929406d8f50634fc050ca62d0fe956e07bfdad930ba31e018b20f64ce4933d3` |
| Delete outcome | `66d26e332878bbd4369462a9e1e48e1441edbd9dd9026de3f3513e5279102789` |
| Exact-UUID absence outcome | `3867096b2a876a754b19e1b02259d0955d4e1b88c9ade18958cdcde875944f94` |
| Workspace listing after deletion | `2ad8b2e98f00d419f71ed9c561646ccc71399d97afeaa947d83f4905beb18cb1` |
