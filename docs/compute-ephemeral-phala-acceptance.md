# Ephemeral Phala CPU — outbound acceptance, 2026-10-20

**A disposable Phala lease passed independent Intel TDX verification, strict
SSH host-key verification, live memory/swap checks and a synthetic guest-file
write/read without using managed KMS. Production remains disabled and the served
trust ceiling remains `Open`.** A second fresh lease passed the same checks
with a different VM, attested instance and SSH key; its channel closed 686 ms
after the original expiry. All four disposable test VMs were confirmed deleted.

This follows the [earlier direct-ingress feasibility test](compute-keyless-phala-feasibility.md).
That test's refused ingress and failed disk reuse remain valid findings. This
implementation uses an outbound channel and a new VM with a new disk for each
lease. It does not qualify Phala's managed KMS, establish persistent-secret
custody, or demonstrate a paid Covenant buyer/settlement flow. The provider
resources were real; the buyer workload and file contents were synthetic.

## Immutable candidate

| Item | Accepted lease 03 / replacement lease 04 |
| --- | --- |
| Explicit model | `phala-cpu-ephemeral-dstack061` |
| Verification profile | `phala_ephemeral_dstack061` |
| Guest image source | `fc04f65493f54f32a47afc96a21d1c6010b97dea` |
| Linux amd64 image digest | `sha256:925122b9c68fe8f385784b3d926685f6e08f59467c5a76fbfe71faa701d81932` |
| Successful image build | [GitHub Actions 39494553288](https://github.com/open-covenant/covenant/actions/runs/39494553288) |
| Verifier policy correction | `2804a5a0a6eadb705d1127ad0d86de51760aedfa` |
| Ephemeral reference SHA-256 | `a517b9e98871e7ff774fe9f788be467fed2565cb4ef3ca7529bf03f0c5e23f9e` |
| Production OS | `dstack-0.6.1` |
| OS image hash | `ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6` |
| Measured pre-launch script SHA-256 | `7a343993a5c15903b17198f7da721512be98f5dd553583ded861ce300c1fdfa3` |
| Lease 03 exact workload hash | `b47fd3779a808b364242278e0034a1a0c9aa0830dbb8e2a58432d7ea825063f6` |
| Lease 04 exact workload hash | `88e7a643bea6bd35c66a94c946593b6a1be57135287ff6ac8bad388462558627` |

Anonymous registry reads independently checked the pinned image manifest,
platform and source metadata. The image metadata helper did not itself verify
CI success; a separate retained GitHub workflow response confirms the successful
build of the same source commit.

The checked-in [wrapper](../agent-os/crates/covenant-compute-protocol/src/phala-ephemeral-wrapper-v3.json)
requires `key_provider: "none"`, omits `key_provider_id`, disables KMS, local key
provision, gateway and tproxy, uses empty features/environment allowlists and
sets `swap_size` to `"0"`. The exact returned app-compose bytes and hash must
match before creation. No sealed environment, Cloud API credential, collector
token, buyer private key or relay buyer token enters the guest configuration.

The measured [pre-launch script](../deploy/compute-attested-cpu/ephemeral-pre-launch.sh)
rejects unmeasured root credentials and host login/debug surfaces, checks
swap/core controls and pulls only the pinned public image. It does not import
the stock provider script or its unmeasured SSH keys. The image enforces
memory-backed `noswap` workspace/runtime volumes, zero guest and cgroup swap,
disabled core dumps and a root-only volatile SSH private host key. See the
[runtime and transport contract](../deploy/compute-attested-cpu/README.md).

## Bounded resource lifecycle

The retained plan allows at most four sequential new resources: three leases
with 900-second application deadlines and a final 360-second lease. Every
attempt uses a distinct VM, disk, job and key set. Failed guests are deleted;
there is no update/restart, disk reuse, implicit replacement or adoption of a
new instance under the old lease. These application deadlines do not by
themselves terminate provider billing.

| Attempt | Result | Cleanup evidence |
| --- | --- | --- |
| 01 | Pre-launch failed closed. Source review identified an invalid query of an uninstantiated systemd SSH template after the last logged operation; no exact error was logged. A template-safe query passed on later guests without weakening the inactive-state requirement. | DELETE 204, exact-UUID GET 404 and workspace-list absence confirmed. |
| 02 | No accepted guest channel. The worker's 32-KiB identity limit was too small for observed dstack identity envelopes; this diagnosis was corroborated by a later 52,803-byte envelope, not by a direct log of the failure. The next image retains a bounded 256-KiB limit. | DELETE 204, exact-UUID GET 404 and workspace-list absence confirmed. |
| 03 | Full quote acceptance, strict SSH, live memory checks, synthetic guest-file write/read and five negative binding controls passed. | DELETE 204 at 20:51:59.950 UTC; exact-UUID GET 404 at 20:52:35.056 UTC; workspace-list absence confirmed. |
| 04 | Fresh quote, strict SSH, repeated runtime checks and new VM/instance/key isolation passed. The active channel closed at 20:58:51.706 UTC, 686 ms after the original application expiry. | DELETE 204 at 20:58:52.113 UTC; exact-UUID GET 404 at 20:59:13.561 UTC; workspace-list absence confirmed. |

Attempt 01 also exposed a private acceptance-helper serialization error:
provision returned `token: null`, and sending that null in create was rejected
with HTTP 422. After confirmed application absence, the helper omitted the
field and created the original planned application. The native implementation
already omits absent/null tokens. This was an explicit rejection, not an
ambiguous create retried under a new identity.

The provider acknowledged scheduled deletion, but its response did not prove
the stored deadline. Explicit deletion and independent absence checks are
therefore retained for each completed attempt. No final provider invoice has
been reconciled, and the planning allowance is not a billing cap.

The two independently verified guests have distinct provider and attested
identities. These identifiers are public evidence pins, not credentials:

| Lease | Provider VM UUID | Provisioned and verified app ID | Verified instance ID |
| --- | --- | --- | --- |
| 03 | `cb3297fe-0cab-4e42-8479-4f79e3b1661a` | `94973e737786a9b7b5b91403263a10c5fdc719f1` | `72cb4d3485ed207922416ac0651b8ac9e077a7d5` |
| 04 | `ba1d0e8a-69eb-45e6-bdf6-f05247bc9378` | `d80ac632811eec234b07867eda73eaa73a59c6d2` | `4133c6ae7adfce615d550e00dfc6d8c3baa215a1` |

## Independent quote and channel acceptance

Lease 03's second quote passed at **20:51:24.817 UTC**, using the actual current
clock, Intel collateral fetched from the verifier's fixed HTTPS allowlist and
the independently retained challenge. Intel signature/collateral verification
passed; platform and QE status were `UpToDate`, with no advisory IDs. Covenant's
complete ephemeral policy accepted the quote.

The verifier checks the pinned launch measurements, debug-disabled state,
complete RTMR replay, exact workload, report-data challenge, `key-provider`
event with `{"name":"none","id":""}`, encrypted ext4 storage, and the
application identity pinned from the independent provision response. The
verified instance is then pinned in the relay. Cloud/RPC identity and relay
registration are untrusted observations until these checks pass.

The first genuine quote had already passed Intel cryptographic checks, but
Covenant rejected it because the verifier incorrectly required the managed-KMS
`os-image-hash` event. In the pinned upstream source, that event is emitted
only after a successful [KMS response](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L2395-L2401).
The explicit ephemeral profile now requires its absence. OS authentication
still checks the complete independent launch tuple: the [kernel, command line
and initrd are measured](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-mr/src/tdx.rs#L458-L478),
including the root hash used by [mandatory dm-verity initialization](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/os/yocto/layers/meta-dstack/recipes-core/images/dstack-initscript/init#L83-L113).
The managed profile still requires its original event. The original rejected
quote was retained; acceptance used a fresh challenge and the current clock.

Only after acceptance did the buyer pin the verified instance and SSH public
host key. OpenSSH used strict host-key checking through the outbound relay;
the relay carried ciphertext and did not establish guest identity. The SSH
command exited 0 and reported:

| Live check | Observed result |
| --- | --- |
| Buyer identity | uid 10001 |
| `/workspace` | Whole tmpfs mount with `noswap` |
| Guest swap | No configured swap devices/files |
| Container swap | `memory.swap.max=0`, `memory.swap.current=0` |
| Core dumps | Disabled |
| Fresh-workspace marker | Absent before writing |
| Private SSH host key | Unreadable by the buyer |
| Synthetic write/read | 81,920 bytes; SHA-256 `4de29bfafed1e1f07ec505e8231cfe487532c4b44dc9994de7888f5ca4f6bd9d` |

The command generated and read these bytes inside the guest. This proves a
guest workspace operation over the authenticated command channel, not an
81,920-byte client upload or live SFTP transfer.

Five offline negative checks used the real quote and actual current time. Each
modified input was paired with a successful unmodified positive control:
changed nonce, SSH key and workload each failed report-data binding; a changed
independent Cloud application pin failed identity matching; removing the
explicit ephemeral profile failed profile selection. All five rejections and
five positive controls passed. They test verifier binding, not real provider
tampering or a production admission path.

## Fresh-lease isolation and expiry

Lease 04's quote passed at **20:54:58.092 UTC**, again with actual-current-clock
Intel and Covenant verification, `UpToDate` platform/QE status and no advisories.
The verified instance and attested SSH host key both differ from lease 03's;
the provider VM UUID also differs. Provider serial logs for both guests record
temporary app-key generation with provider `none` and formatting a new encrypted
disk. Those logs corroborate the lifecycle; they are not independent proofs of
the provider's physical storage disposal.

The second strict SSH session repeated every runtime check in the table above,
found the previous workspace marker absent and wrote/read the same synthetic
81,920 bytes inside the new guest. It then held the channel open across the
original lease deadline, **20:58:51.020 UTC**. OpenSSH exited 255 at
**20:58:51.706 UTC**, 686 ms later and before the relay's 300-second stream cap.
The retained comparison confirms distinct VM, verified instance and attested
SSH key, fresh workspace and matching guard observations.

This demonstrates closure of the authenticated ciphertext channel at expiry.
Relay and guest expiry checks, scheduled provider deletion and the local cleanup
guard were all active. The client reported a closed/unknown relay transport
outcome; this test does not isolate which control ended the channel and does
not independently prove guest-process termination or the guest watchdog alone.
The cleanup guard received DELETE 204; a subsequent exact-UUID GET returned
404. The final authenticated workspace listing omitted all four test VMs and
still included the original diagnostic VM, which was left unchanged. The local
relay expired, and the temporary HTTPS tunnel was terminated.

## Activation boundary

This proves a bounded Phala-hosted, no-KMS execution channel for the pinned
candidate. It does not provide persistent-secret recovery or qualify the
managed KMS. The [managed-KMS review](../deploy/compute-attested-cpu/kms-policy-review.md)
and [production acceptance gates](compute-hardware-acceptance.md) remain in
force. `production_qualification` is false in the accepted verifier output.

At this isolated acceptance, native allocation persisted the exact lease and
provision/create/delete intents, but native readiness and buyer transport were
disabled. The subsequent implementation is described in the
[native lifecycle contract](compute-phala-lease-lifecycle.md#ephemeral-no-kms-native-integration).
It adds independent buyer admission, signed coordinator relay, cancellation,
confirmed teardown and metered settlement. Implementation and local tests do
not turn this historical isolated test into a native paid acceptance.

Production still requires acceptance of that complete paid path on the deployed
candidate and a bounded operator creation policy. A successful isolated
acceptance is not an advertised production lease or a change in served trust.

The two subsequent [native paid attempts](compute-native-phala-acceptance.md)
passed independent buyer admission and 81,920-byte client uploads, then failed
their deletion-confirmation gates. The first exceeded 60 seconds; the
October 21 retry, after deployment of source `6598fe15`, confirmed absence
18.926 seconds after its fixed 300-second cutoff. Both original VMs were
subsequently confirmed absent, buyer credit restored and escrow returns
finalized. Both failures remain recorded, both one-use allowances are consumed,
and the operator is stopped with no advertised Phala capacity. The subsequent
poll-only deletion repair is local and undeployed. No third paid attempt is
authorized, and successful paid production qualification remains outstanding.

## Privately retained evidence hashes

Raw quotes, collateral, API responses, account metadata, private keys and
authorization material remain private. These SHA-256 hashes identify selected
retained artifacts without publishing their contents.

| Artifact | SHA-256 |
| --- | --- |
| Bounded acceptance plan v3 | `3c63cfb7df728f4c61cc779daec642477f29ddfdbf1f966e5263231b17fcaeea` |
| Image verification | `68e8f055ceff09b93ef971b7ca95312ffd5c7021c7aa3fce0a28e6361653404d` |
| Lease 01 confirmed cleanup | `6506a4475a8cd9e7ca0e3535091cac4debce4925a7a7d4e971978e0833866027` |
| Lease 02 confirmed cleanup | `68cca8946c6205857ddebc06f510bf218cff648b3bbedf5069bac8db6797d2e4` |
| Lease 03 accepted raw quote | `40f4662da9121ee29f027bff6f51a7e8f450edc5afc618c884afb0e75d50db8e` |
| Lease 03 accepted verification | `8c6a5a2eda02fe6fc8795ee3d4d40be9d0163c8ecc0048ba90249972a16eec3f` |
| Lease 03 SSH observations | `57b873cc7f21dbee68ab6e87db0c8306b9a2819468e30890aaa7a502254c0d61` |
| Lease 03 five negative checks / five controls | `db1c6ae9c899f3b3b1c5f48e649e8228e3413e4dcaa4095e665a563eabf24fda` |
| Lease 03 confirmed cleanup | `782c45a8cbeaa3744f9704ca1655f6d3877ac534428d67b7634b596b0f86a6d9` |
| Lease 04 accepted raw quote | `96ba04b2759e709b039393b33d81a33fc999dc24729d9cbbf584abadef1abb8d` |
| Lease 04 accepted verification | `d36da46e6976893b9c110c57befb1034ba1a2625ab780b8c28e72e833934c202` |
| Lease 04 SSH observations | `57b873cc7f21dbee68ab6e87db0c8306b9a2819468e30890aaa7a502254c0d61` |
| Lease 04 SSH expiry outcome | `eb0b6a55605a2b33b3ece9e63a42c265ea68608eac1574d34db5d94f3160982f` |
| Lease 03/04 isolation and expiry comparison | `c04c9bb89b4c33459ad3d1d5719806dedab8b2e44c48f869f66e4ef97c736cff` |
| Lease 04 confirmed cleanup | `8961c229ddcf19d8571d5731576641e9aa5ba8e2db98c57e2cb221f74df1e982` |
| Final four-VM cleanup summary | `eeb95c1c8183ffd115fc60d83ed37b953062e04a151f5e211dddc24434a35b0c` |
