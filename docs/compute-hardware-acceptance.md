# Measured CPU lease acceptance

Package 11 now includes a Phala CPU lease lifecycle, signed per-lease workload
pins, automatic challenge/evidence collection and a buyer path for independent
verification, SSH probing and workspace restore. The served trust ceiling
remains **`Open`**. Managed-KMS creation remains disabled; ephemeral creation
requires an explicit bounded allowance and remains disabled by default.
The coordinator integration is deployed at `6598fe15663ad88fe256ad739ded703d05e1ad43`.
Both paid native attempts passed independent admission and real client upload,
then failed deletion confirmation: the first exceeded its 60-second gate; the
October 21 retry exceeded its fixed 300-second cutoff by 18.926 seconds. Both
original VMs were subsequently confirmed absent, buyer credit was fully
restored, and both original on-chain escrow refunds finalized. See the
[native acceptance record](compute-native-phala-acceptance.md). Successful paid
settlement and general production capacity remain unqualified.

A separate [ephemeral Phala profile](compute-ephemeral-phala-acceptance.md)
now allocates a fresh VM/disk per lease and connects outbound using an
independently attested SSH key, without managed KMS. Its bounded live diagnostic
is distinct from the native paid lease flow. The native integration now verifies
operator and buyer admission, relays attested SSH, and gates successful receipts
on confirmed teardown. Neither failed native attempt establishes a successful
paid lease, even though admission, upload and eventual deletion passed.

The [lease lifecycle guide](compute-phala-lease-lifecycle.md) records the exact
configuration and buyer commands. The [hardware contract](compute-hardware-attestation.md)
describes verification and grant rules. This plan separates the evidence
already obtained from the decisions and live checks still required.

## Established evidence

On 2026-10-20, the user-created diagnostic Phala CVM ran Covenant's isolated
SSH service and collector on production dstack 0.6.1. A genuine TDX quote passed
strict Intel signature and current-collateral verification with `UpToDate`
platform/QE status and no advisories. The measured workload, complete launch
tuple and full Covenant challenge matched. SSH/SFTP, buyer isolation, replay
and endpoint/key/workload substitution checks passed. Both temporary services
exited at the original signed expiry; an existing SSH session closed 144 ms
after the cutoff. See the [diagnostic evidence](compute-hardware-diagnostic-evidence.md)
for immutable identities, timestamps and artifact hashes.

That diagnostic was not a paid coordinator lease, workspace grant, settlement
or provider deletion. Its CVM
`32ed28e4-0a6f-4dff-a9bf-ba84c38d6e9f` was user-created and retained. It is not
owned by the new adapter journal and must not be adopted or deleted by inference.
Service shutdown is not proof that provider billing stopped. The diagnostic
also recorded inconsistent provider operation status; its historical evidence
does not claim that this discrepancy was reconciled.

The production reference is independently derived from official dstack source
`7f143148cc49f4a09799958835d310e050137cb3` and release artifacts, for the reviewed
1-vCPU, 2-GiB lite configuration. The OS image hash is
`ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6`.
The [measurement recipe](../deploy/compute-attested-cpu/measurement/README.md)
reproduces MRTD and RTMR0–2 without trusting values reported by the guest.
Unknown or mixed launch tuples, malformed runtime logs, hardware debug,
unacceptable TCB or advisories remain rejected.

Management account access is now working. Authorized reads of the diagnostic
app's KMS information, device allowlist, revisions and pending updates all
returned HTTP 200. Authenticated CPU inventory and CVM listing also succeeded.
The CPU catalog returned `tdx.small`, 1 vCPU, 2048 MiB, a 20-GB default disk and
an hourly-rate string of `0.058000`. This is a catalog component observed at
that read, not an all-in invoice cap, a permanent quote or spending approval.
The earlier signed-in total estimate must not be replaced with this component.

The [memory diagnostic](compute-memory-diagnostic-evidence.md) records the
subsequent tested image and exact update of that same CVM. On 2026-10-20,
the new workload passed fresh Intel TDX verification and attested SSH checks:
128-MiB `noswap` workspace tmpfs, empty guest swap, zero cgroup swap allowance
and usage, and disabled core dumps. A synthetic 1-MiB file was created, read
and removed inside the guest. Docker CI separately passed real SSH transfer,
capacity, restart-erasure and swap-watchdog negatives. These observations do
not qualify KMS policy, paid lifecycle integration or a production release.

## Remaining production gates

1. **KMS and administrative policy: production qualification rejected.**
   Authenticated metadata from managed prod5, the account-selected managed
   prod7 endpoint, and all 16 registered Base/Ethereum KMS nodes reported
   `os_image_verification=false` on 2026-10-20: 18 of 18 assessed endpoints.
   Account evidence confirms the diagnostic app uses off-chain KMS; its prod5
   compute host does not imply a prod5 KMS endpoint. The managed model requires
   provider remediation with measured configuration
   and governance evidence, or a separately qualified customer-controlled
   KMS deployment. See the [KMS policy review](../deploy/compute-attested-cpu/kms-policy-review.md).
   Accepting the current managed-policy threat model can authorize only a
   finite synthetic pilot at `Open`; it is not an alternative production-trust
   qualification and cannot establish persistent-secret exclusivity.
   The separate ephemeral profile avoids that key-release dependency through
   fresh single-boot storage and an attested outbound channel. Its live proof
   is recorded separately; production still requires complete native relay
   admission, ownership/lifecycle integration and release review. It makes no
   persistent-secret custody claim and does not relax managed-KMS verification.
2. **Production release qualification.** The new digest-pinned diagnostic
   image passed real SSH/SFTP container smoke and live mounts/cgroup/swap/core
   checks on the existing CVM. Its source, registry bytes and runtime evidence
   are retained in the memory diagnostic record. This is not a production
   promotion, signed release provenance or acceptance of another image.
   Complete the release review against the ultimately accepted KMS/profile
   and paid lifecycle evidence before publishing production capacity.
3. **Bounded paid lifecycle acceptance.** Approve separate native Compute USDC
   and provider USD exposure, exact placement, one new resource, finite
   lifetime and cleanup ownership. Exercise the integrated lease, evidence,
   buyer channel, synthetic workspace transfer, settlement and confirmed
   provider deletion with the original identities. Both native attempts are
   recorded as failed and fully refunded. The cleanup/result-deadline repair
   is deployed, but the subsequent poll-only deletion repair remains local and
   needs release qualification. A new paid test requires fresh authorization;
   no third attempt is approved. Preserve both failed envelopes and consumed
   allowances. The test node is stopped and no standing Phala production
   capacity is enabled.
4. **Trust-release decision.** Keep `Open` during diagnostics. A stronger
   workspace floor must continue to reject. Any future ceiling increase is a
   separate reviewed code/release change for an accepted profile, not an
   environment switch or a consequence of valid quote cryptography.

Production dstack excludes native guest `sshd`; the measured pre-launch script's
root-key import path alone does not establish reachable native SSH. The earlier
log reported zero imported keys. KMS authority, mutable account configuration,
privileged pre-launch behavior and the reviewed wrapper's other settings still
belong in the policy decision. Application isolation checks do not prove
provider-independent custody of persistent secrets.

## Guest memory controls

The current source hardening makes the SSH workspace a 128-MiB `tmpfs` with
`noswap`, rather than a persistent workspace volume. SSH `/run` and `/tmp`
are also `noswap` tmpfs mounts, limited to 8 MiB and 16 MiB. The shared
`/host-public` mount uses a new 1-MiB `noswap` tmpfs volume name so an update
cannot silently reuse the older disk-backed volume; the collector mounts it
read-only. It carries the public host key and runtime status, not the private
SSH key. The collector's replay journal remains durable.

SSH and collector have equal memory and memory-plus-swap limits
(768 MiB and 256 MiB respectively), and core soft/hard limits of zero.
The runtime guard also rejects a piped or unknown kernel core handler,
active guest swap, nonzero cgroup swap allowance/usage, unexpected mount
types/flags/sizes and substituted kernel metadata. SSH checks before key
generation and keeps checking through its watchdog; a failed check stops
SSH and invalidates its status. The collector requires fresh status bound
to the same lease and host key before and after guest RPCs/quote collection.
Unsupported runtime controls fail closed.

The signed app-compose wrapper explicitly contains **`"swap_size": "0"`**.
Omission, numeric zero or another value is rejected. The node compares the
provider's normalized Compose hash to the exact buyer-signed workload before
creation; it cannot silently accept changed swap settings. The live diagnostic
confirmed this exact wrapper hash and the actual runtime controls. They depend
on the measured guest and do not resolve the separate KMS production rejection.

## Review record before one live attempt

Prepare a concrete record naming the buyer/operator, job and lease generation,
exact guest digest and signed workload hash, KMS CA and policy decision, selected
production OS/KMS/legacy Teepod ID, current instance type/rate, finite expiry,
provider account, cleanup owner and evidence destination. Select a currently
available Teepod through the authenticated account; a remembered diagnostic
placement or the newer `node_id` is not a substitute for `Teepod.id`.

Record native Compute's maximum price and durable daily-budget allowance
separately from the provider's one-resource USD planning reserve. Include boot,
probe and cleanup time, storage/ancillary billing and the response if deletion
fails. A provider planning reserve is not an enforceable invoice cap. The old
**0.75 USDC x402 authorization does not authorize this Phala resource or lease**.
This document supplies no new numerical spending approval.

For a current managed-KMS pilot, the record must explicitly restrict the test
to synthetic data at `Open`. It cannot authorize stronger production trust.
The machine-readable creation allowance limits the operator, instance type,
rate, runtime, expiry and one durable planning attempt. It is an execution
guard, not the KMS policy acceptance record: it has no CA, app or workload
acceptance fields. Both records are needed before considering creation, and
`COVENANT_PHALA_ALLOW_MANAGED_KMS_CREATION` remains `0` until that decision.

## Live acceptance sequence

1. Complete current source/image qualification, policy review and explicit
   spending/cleanup authorization. Verify account inventory, production
   placement and the live rate again. Preserve existing journals and budgets.
2. Prepare the buyer's exact workload and signed envelope offline, then inspect
   their scope and price. Submission must occur within 120 seconds of issuance
   and before the original signed expiry. Never refresh a stale signed request
   or substitute a new job automatically.
3. Submit once through the durable buyer budget. Retain the original envelope,
   reservation, provisioned app identity, create intent and resulting CVM UUID.
   On an uncertain submit/create, reconcile that original identity; do not buy
   a replacement or clear a reservation to obtain progress.
4. Require current lease readiness and automatic collection of a fresh
   coordinator challenge, genuine quote and current Intel collateral. Retain
   local verification and coordinator acceptance. Re-read evidence using the
   independent buyer verifier, then prove possession of the attested SSH key
   through the typed, hostname-validated TLS transport.
5. Run `probe-phala` and transfer only a synthetic workspace permitted at
   `Open`. Confirm exact bytes and remove them. Re-read lease/scope and verify
   evidence immediately before upload. Demonstrate that stronger-floor
   workspace access remains refused.
6. Exercise wrong buyer/operator, future issuance, expired/consumed challenge,
   changed key/endpoint/transport/workload, unacceptable collateral, close and
   original-expiry rejection. Test restart after durable intents and challenge
   consumption without creating a second resource or extending the expiry.
7. Close the original lease and reconcile native settlement/budget by original
   IDs. Confirm deletion of the journal-owned CVM through authenticated GET
   absence, then retain storage/billing evidence. HTTP 204, stopped containers
   or an unreachable endpoint alone do not establish completed cleanup. Keep
   unresolved creation, payment or deletion obligations durable.

## Validation status

The historical real diagnostic above is complete within its recorded scope.
Before the current memory/swap hardening, the lifecycle changes passed
310 protocol tests, 276 buyer library tests
(two existing ignored cases), 45 buyer CLI tests, 17 coordinator hardware tests,
seven attestation binding tests, 23 node Phala tests, 11 node lease tests and
four node admission tests have passed. The final matcher test also passes:
the signed operator wins over cheaper compatible supply, and an unavailable
target never falls back. Node tests passed again with the unresolved-resource
shutdown checks. The two node unit-test filters overlap
and must not be added as unique tests. All 37 collector/client Python tests
passed without skips. Cases cover unknown-create recovery, cleanup after
configuration drift, expiry, encryption and creation allowances. The environment
inventory guard, workspace formatting, full repository guard scripts and
documentation link/diff checks passed. Clippy passed with warnings denied for
all targets in the five affected crates, including both executable entry paths.
The earlier source head passed Rust, landing and guest-image CI. Native
packaging is now consistently version 0.5.0; no release tag was created.
At source `232bf28755387a4cbf803a53b02fc0880e0eb7c7`, all 47 Python tests
passed without skips, focused protocol tests and repository guards passed,
and both image workflows passed the actual Docker smoke. Registry
manifest/config bytes and labels were independently checked before the
same image's live memory/swap diagnostic. Rust CI, landing CI and all four
native platform builds also passed at that source; the memory diagnostic
record links those runs. These results do not establish paid acceptance.
No paid integrated lease, customer-data transfer, production trust promotion
or provider-deletion acceptance has run for this change.
