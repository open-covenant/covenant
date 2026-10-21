# Hardware evidence and release status

The `covenant-compute-attestation` verifier checks raw AMD SEV-SNP, Intel
TDX/DCAP and NVIDIA H100 CC evidence cryptographically. Certificate chains,
signatures, reference measurements, TCB floors, debug/secure-boot/CC policy,
fresh nonces and report-data bindings all participate. Decoding a report or
observing `tee_capable` never earns trust. The compiled served ceiling remains
**`Open`**, including when a genuine report has evidence class `Attested`.

A genuine Covenant-bound production Phala CPU diagnostic has passed strict
verification, SSH/SFTP, isolation, binding negatives and original-expiry checks;
its [evidence record](compute-hardware-diagnostic-evidence.md) states the exact
scope. The new [Phala lease integration](compute-phala-lease-lifecycle.md) is
implemented locally but has not completed a paid lease or production acceptance.
Managed KMS policy, a new qualified guest image and the integrated live test
remain explicit [acceptance gates](compute-hardware-acceptance.md).

## Binding and verification

The Covenant wrapper binds the full UUID, buyer, assigned operator, lease
generation, endpoint, typed SSH transport, host key, workload SHA-256 and lease
expiry into guest report-data using SHA-512 and a Covenant-specific domain.
The GPU nonce uses SHA-256 under a separate domain. Combined evidence also
commits the exact GPU report and chain into the guest report-data through
`guest_report_data_for_gpu`, preventing substitution of a different GPU report.

The coordinator consumes each challenge durably before verification, rejects
clock rollback and corrupt journals, bounds hardware work to two blocking
workers without an unbounded wait queue, and rechecks the current lease before
persisting a result or issuing a protected workspace grant. Blocking journal
work uses the same admission bound. One verified GPU cannot be assigned to
different operators during a live verdict.

For the production dstack 0.6.1 profile, the verifier requires the complete
reviewed launch tuple, V2 runtime event replay, measured app-compose, instance,
OS and key-provider bindings. Runtime-register event types cannot be relabelled
as opaque firmware to hide duplicate identity or boundary events. Expected
measurements come from the [independent official-artifact replay](../deploy/compute-attested-cpu/measurement/README.md),
not from the quote under examination. Strict Intel `UpToDate`, advisory and
debug policy remains unchanged. Reference images and vendor roots retain
pinned provenance in [UPSTREAM.md](../agent-os/crates/covenant-compute-attestation/UPSTREAM.md).

## Coordinator modes and HTTP contract

Set `COVENANT_COMPUTE_HARDWARE_LEASE_PINS=1` for signed per-lease mode. The buyer's
signed `phala_lease` input supplies the workload, operator, generation and
absolute expiry. There is no global-workload fallback for unsigned legacy jobs.
The legacy diagnostic mode instead uses
`COVENANT_COMPUTE_HARDWARE_WORKLOAD_SHA256`. A nonempty legacy pin and signed
per-lease mode are mutually exclusive. With neither configured, hardware
routes are not registered.

Journal configuration is part of the integrity boundary. A changed legacy pin
or a switch between modes requires a separately reviewed namespace; do not
reset, overwrite or delete financial/evidence state to bypass a mismatch.

- POST `/federation/hardware/{job}/challenge` accepts `{"host_key_index":0}`.
- The assigned operator POSTs the challenge and raw guest/GPU evidence to
  `/federation/hardware/{job}/evidence`.
- The buyer or assigned operator GETs `/federation/hardware/{job}/verdict`.

Ed25519 request headers bind the method, path, exact body and a 30-second
timestamp window. The lease must be accepted and current, not closing, not
before signed issuance, and before its deadline, paid window and signed expiry.
Phala additionally requires the exact signed operator/generation, user
`covenant`, port 443 and the instance-specific `-1022` endpoint on the fixed
reviewed gateway, with typed TLS transport.

Challenges last at most 120 seconds; verdicts at most 300 seconds. Both are
bounded by the current lease window. A failed verification consumes its
challenge; a pre-admission overload does not. A deliberately requested new
challenge does not extend the lease or authorize a new purchase. Unknown
collector/submission outcomes must first be reconciled from the original
private evidence run.

Raw evidence and verdict metadata are stored privately and atomically with
fsync. Private SSH keys and prompts do not belong in this journal. Saved
verdicts recompute the original result and revalidate vendor collateral at the
current read time, without extending the original challenge window. Evidence
HTTP responses use `Cache-Control: no-store`.

## Buyer and provider integration

`hardware::HardwareClient` independently re-verifies raw vendor evidence
against the buyer's original signed lease/workload. Coordinator labels alone
are insufficient. `verified_known_hosts` is only a formatting helper. The
buyer SSH client validates gateway TLS through native certificate/hostname
checking, then uses strict OpenSSH host-key checking for the actual attested
guest key. It accepts no provider-supplied shell `ProxyCommand` and ignores
ambient SSH configuration, agents and multiplexing.

The Phala adapter provisions one CPU resource for the signed
`phala-cpu-dstack061` profile. It durably records mutation intent, preserves
unknown outcomes, collects challenge-bound evidence, and recovers cleanup
before admitting fresh work. It does not adopt the earlier user-created
diagnostic CVM. New creation requires both an explicit managed-KMS switch and
a finite one-use allowance, and remains disabled pending acceptance review.
The [lifecycle guide](compute-phala-lease-lifecycle.md) documents configuration,
buyer commands and original-identity reconciliation.

The separate `phala-cpu-ephemeral-dstack061` executor uses an explicit no-KMS
profile, a fresh VM/disk per lease, and an outbound signed coordinator relay.
Node and buyer independently verify the guest, signed workload and exact
app/instance/host-key binding before access. A provider cannot select this
profile for a managed-KMS order. Creation is disabled by default and requires
its own one-use allowance. The deployed coordinator at `6598fe15` exposes this
opt-in capability; it does not provide standing Phala production capacity.

Workspace restore verifies the current lease and evidence before local restore,
then rechecks scope and verifies again before upload. Stronger workspace floors
still reject at the `Open` served ceiling. Valid cryptography is not an override.
The current Vast broker retains legacy access (`ssh: None`); it is not converted
into measured capacity. Optional transport metadata preserves legacy wire bytes
when absent. This work does not activate confidential GPU inference or imply
that arbitrary program outputs are correct.

## Validation and release boundary

The earlier verifier baseline at
`72f66142f407690024c6a0ca4a3bdaf148ef8766` passed 1,419 library tests across six
crates and 76 attestation integration tests, with existing opt-in cases left
ignored, plus formatting, documentation guards and all-target Clippy. Those
results describe that baseline, not the current lifecycle changes.

An earlier integration checkpoint included 310 protocol tests, 276 buyer library tests (two
existing ignored cases), 45 buyer CLI tests, 17 coordinator hardware tests,
seven attestation binding tests, 23 node Phala tests, 11 node lease tests and
four node admission tests passed. The node unit-test filters overlap; these
are not an additive unique-test total. All 37 collector/client Python tests
passed without skips, as did the environment inventory and documentation checks.
Subsequent source/image qualification and live results are recorded in the
[native acceptance report](compute-native-phala-acceptance.md). Both paid
attempts verified genuine guest evidence and client uploads but failed their
provider-deletion deadlines. The October 21 retry, on deployed source
`6598fe15663ad88fe256ad739ded703d05e1ad43`, confirmed deletion 18.926 seconds
after its fixed 300-second cutoff. Both failures, full buyer-credit restoration,
finalized escrow returns and eventual exact-VM deletion remain recorded.
The local, undeployed poll-only deletion repair passed 52 Phala, 13 lease and
four admission tests, strict node Clippy and 224 repository guards. It preserves
the original cleanup cutoff and stops blind DELETE resends after an accepted
or unknown outcome. These local checks do not qualify a paid release. No third
attempt is approved; there is no successful paid Phala lease, operator payout,
stronger-floor workspace grant or production attested-rental release.

The managed KMS has authenticated `os_image_verification=false` and mutable
off-chain authority. Working account access resolves the credential gap, not
that policy gate. See the [KMS review](../deploy/compute-attested-cpu/kms-policy-review.md).
Any future ceiling increase requires explicit evidence review and a separate
code/release change. A later GPU release also needs current CC firmware
allowlisting, measured local-device collection and secure guest-to-GPU custody.
