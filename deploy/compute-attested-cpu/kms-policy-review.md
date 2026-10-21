# KMS policy review

`kms_policy.py` is an optional, read-only operator tool. It is not installed in
the guest image and does not change attestation admission or the Open ceiling.
It requires Python with `cryptography>=42`.

Supply an owner-only JSON plan with exactly these fields:

```json
{
  "schema_version": 1,
  "app_id": "<40 lowercase hex characters>",
  "workload_sha256": "<64 lowercase hex characters>",
  "expected_ca_spki_hex": "<182 lowercase hex characters>"
}
```

The CA pin must come from separately reviewed app-compose bytes and the
authenticated `key-provider` runtime event. Do not derive an expected pin from
the endpoint being checked. The app/workload fields identify the review;
`GetMeta` does not prove authorization of either value.

```sh
python3 deploy/compute-attested-cpu/kms_policy.py \
  --plan /private/path/kms-review-plan.json \
  --output /private/path/kms-meta.json
```

The output directory must be owner-only; both output files must be new. The
tool authenticates the fixed prod5 KMS endpoint against the supplied CA public
key, exact DNS name, certificate validity, signing usage, and `kms:rpc` role
before sending `GET /prpc/KMS.GetMeta`. No client credential, key-release RPC,
redirect, retry, signing, account mutation, or quote request is used. TLS uses
explicit pinned-key verification rather than public-Web-PKI trust. The embedded
certificate attestation is retained but is **not** marked hardware-verified.

The raw metadata and a separate review record include body hashes, the peer
certificate, times, authentication result, and policy gaps. A successful
collection exits **2** because it does not qualify production policy. Failure
also preserves captured evidence. Positive `is_dev=false` or
`allow_any_upgrade=false` fields do not establish an immutable authorization
policy. Missing OS-image verification is never interpreted as enabled.

## Production qualification result: rejected on 2026-10-20

The read-only assessment completed with **`rejected_for_production`**.
The initial 17 endpoints were checked at 18:44:11–18:44:15 UTC, followed by the
account-selected managed prod7 endpoint at 18:54:45–18:54:46 UTC.
All **18 assessed endpoints** returned authenticated
`os_image_verification=false`: managed prod5 and prod7, all eight registered
Base nodes, and all eight registered Ethereum nodes. Each connection passed the separately
obtained registry CA pin, direct certificate signature, exact DNS name,
certificate validity and `kms:rpc` role checks before the metadata GET.
The remaining 20 managed nodes were not surveyed. Switching this application to
either hosted on-chain service therefore does not resolve the observed gate.

The current CVM account record selects
`https://kms.dstack-pha-prod7.phala.network`, despite its compute host being
prod5. That exact endpoint was matched to registered node `kms_QEKvzKbo`
before connection. Managed prod5 and prod7 share the separately pinned PHALA
CA root; root identity does not identify which endpoint the account selected.
The repository helper above remains fixed to prod5; the additional endpoint
survey used a separate bounded collector with the same authentication checks.

The private `qualification-result.json` records every endpoint, observation
time, body/certificate hash, authentication result and qualification limit.
Its SHA-256 is
`832e114001bb764266dbb7fbdf050e6dc5b8b1b61ab5cbc8e62f7d6e0b915b9f`.
It supersedes but preserves the original 17-endpoint report.
The source lists were the official [KMS registry](https://cloud-api.phala.com/api/v1/kms),
[managed nodes](https://cloud-api.phala.com/api/v1/kms/phala/nodes),
[Base nodes](https://cloud-api.phala.com/api/v1/kms/base/nodes) and
[Ethereum nodes](https://cloud-api.phala.com/api/v1/kms/eth/nodes), using Cloud
API version `2026-06-23`. Registry/Cloud inspection statements are not independent
finalized-chain verification. No key-release or quote RPC, account/contract
mutation, deployment, signing or spend was performed.

An authenticated account refresh still reported `is_onchain_kms=false`.
No supported account operation to enable `image.verify` was found in the
reviewed Cloud API schema, official SDK or current KMS documentation. It is
a [KMS server configuration field](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/src/config.rs#L27-L43);
Phala controls that configuration for the hosted endpoints. This finding
does not rule out undocumented provider administration, but an app owner
cannot use the documented on-chain Compose/device controls to enable it.

To retry qualification, Phala must provide a pinned endpoint with image
verification enabled and evidence binding its effective configuration and
authorization backend to reviewed measured code. Verify current attestation,
collateral/TCB, TLS-key binding, manifest/config preimages, root bootstrap and
replication policy, and relevant app/global contract governance. Then perform
fresh acceptance with that authority. The alternative is a separately planned
customer-managed KMS root and authorization deployment, with new infrastructure,
governance, CA/workload pins and acceptance; none was created by this review.

The collected TLS leaves contain embedded attestation, but it was not marked
hardware-verified. The pinned [RA-TLS verifier](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/ra-tls/src/attestation.rs#L38-L94)
can verify hardware evidence and its binding to the TLS public key. That alone
does not establish the effective image-verification or authorization settings:
their measured configuration preimages and enforcement must also be checked.
Enabling a check later cannot prove that existing shared-root or derived keys
were never exposed under an earlier policy; key continuity needs explicit review.

### Provider remediation request (prepared, not sent)

> We are qualifying Covenant's attested CPU lease path on Phala dstack 0.6.1.
> On 2026-10-20 at 18:44 UTC, managed prod5 and all 16 registered Base/Ethereum
> KMS nodes returned `os_image_verification=false`; our account-selected managed
> prod7 endpoint returned the same at 18:54 UTC. All 18 connections were
> authenticated against separately pinned CA roots. Our production gate
> requires this check to be enabled.
>
> Please provide a supported KMS endpoint with `image.verify=true`, its exact
> CA/root identity, and the following verification material:
>
> - Current KMS attestation, event log and collateral, bound to its actual TLS
>   public key; expected launch measurements and source/container image mapping.
> - Exact measured manifest and effective configuration preimages proving image
>   verification and authorization settings, including image-download integrity,
>   chain/contract identity, RPC trust/finality settings and config mutation powers.
> - Root bootstrap/replication evidence, all permitted KMS measurements/devices,
>   and the administrator/upgrade powers that could change key-release behavior.
> - For on-chain authorization, deployed app/global contract implementations,
>   owners, upgrade authorities and OS/device/workload/TCB rules at a named
>   finalized block, including any override or emergency path.
> - The supported migration steps and key-continuity implications for our app
>   using managed off-chain KMS (the selected prod7 endpoint); state whether a
>   new root/application identity is required to avoid relying on keys released
>   under the earlier policy.
>
> We can then independently verify the evidence and run one separately bounded
> lease lifecycle acceptance. A metadata flag or contract allowlist alone is
> insufficient to qualify the effective measured key-release path.

## Qualification paths and retained account evidence

For the reviewed Phala managed KMS, the direct authenticated response reported
`os_image_verification=false`, `chain_id=0`, and no contract address. The official
public KMS registry's CA key matched the application's measured provider key.
This identifies the managed KMS; it does not publish its complete workload
allowlist, authorized account administrators, or future policy changes.

Resolve the policy choice through an explicitly reviewed path. The second
path permits only a bounded synthetic pilot, not stronger production trust:

1. **Customer-controlled policy:** obtain authenticated KMS deployment/config
   provenance, current attestation and enabled OS-image verification; bind its
   authorization backend to the actual chain and contracts. Review app and KMS
   owners, implementation/code, approved Compose hashes, device/TCB policy and
   all mutation powers at one finalized block. Decide acceptable governance
   and refresh requirements. `disableUpgrades` blocks implementation upgrades
   only; it does not disable owner-operated Compose/device/TCB setters.
2. **Bounded managed-policy acceptance for a synthetic pilot at `Open`:**
   an authorized Covenant operator must
   explicitly accept Phala's centralized, mutable policy and disabled KMS
   image check for the named CA pin, app, exact workload, device/lease scope
   and finite expiry. Retain authenticated account evidence of current
   deployment/revision and device policy, responsible administrators, and the
   decision. Covenant must independently verify the full production launch
   tuple, Intel TCB/advisories, fresh challenge and signed per-lease workload
   pins. Limit exposure to the approved lease; do not claim immutable policy
   or provider-independent persistent-secret exclusivity.

This helper cannot make that acceptance decision and has no override that
turns collected metadata into a passing policy verdict.

Read-only account routes include `/api/v1/apps/{app_id}/kms-info`,
`/device-allowlist`, `/revisions`, and `/pending-updates`. The signed-in Phala
app overview/instance settings can support that review. Account UI evidence
does not turn centralized governance into independently immutable policy.

The authorized account read on 2026-10-20 returned HTTP 200 for all four
routes for the diagnostic app
`d5f0ef708c68c52916a7b0da313ae416eb0b980d`. It confirmed
`is_onchain_kms=false`, with no chain, KMS/app contract, deployer, or Safe
addresses. Device policy returned `allow_any_device=null` and `devices=[]`;
these represent unavailable on-chain policy information, not a verified
deny-all or allow-all rule. Revision history contained the starter Compose
hash `e611067a2f2dc3b5c0c2bedfccc0b3f4567f0044c22a30213868b936db1c8d61`
and the reviewed collector hash
`c4428b75f358f9f3295dea7fa73c59ab590344ee42717e7080c6c6167ea5ac97`.
The pending-update list was empty at collection time. Raw account responses
and hashes are retained privately; no account credential is part of the
source or evidence output. These reads resolve the account-access gap, but
do not publish the managed KMS's full authorization rules or administrator
powers, and do not alter the disabled-OS-verification finding above.

## Source basis

- [Phala Cloud versus on-chain KMS](https://docs.phala.com/phala-cloud/key-management/cloud-vs-onchain-kms)
  documents the managed policy authority.
- Pinned dstack `7f143148cc49f4a09799958835d310e050137cb3`:
  `dstack/cert-client/src/lib.rs:34–46` requires `kms:rpc`, because the root also
  anchors application certificates; `dstack/dstack-util/src/system_setup.rs:2412–2424`
  compares the KMS key against the app-compose pin.
- `dstack/kms/src/main_service.rs:387–475` gates release through authorization
  and derives keys from app/instance identity rather than Compose hash;
  `:534–562` emits metadata. `DstackApp.sol:136–223` distinguishes implementation
  upgrades from mutable policy setters.

Run `python3 -m unittest discover -s deploy/compute-attested-cpu -p
'test_kms_policy.py' -v` with `cryptography>=42` to exercise certificate and
transport rejection tests. The normal dependency-free suite skips these
optional crypto tests when that package is absent.

## OS verification and the limits of on-chain app ownership

The pinned dstack source explicitly warns that when `image.verify=false`,
`os_image_hash` is caller-supplied and the `allowedOsImages` check does not
enforce the booted OS identity. The KMS returns without verifying the image
when this flag is false, even after a successful authorization decision.
Selecting an on-chain app contract does not enable that independent server
check. See the [configuration warning](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/kms.toml#L80-L86)
and [release-path check](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/src/main_service.rs#L369-L410).

Customer ownership of `DstackApp` controls its Compose/device allowlists and
optional `UpToDate` requirement. The standard app policy does not check an
exact OS measurement or advisory IDs. The separate `DstackKms` owner controls
the global OS allowlist, permitted KMS measurements/devices, factory
implementation and shared-contract upgrades. App ownership does not transfer
control of the KMS server or root keys. A custom app policy can inspect
additional measurement/advisory fields, but its implementation, authenticated
inputs and enforcement require their own review.
Sources: [app policy](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/auth-eth/contracts/DstackApp.sol#L190-L216),
[KMS authority](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/auth-eth/contracts/DstackKms.sol#L102-L305)
and [policy inputs](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/kms/auth-eth/contracts/IAppAuth.sol#L36-L45).

These are properties of the reviewed source, not proof that the observed
managed KMS runs those exact bytes. Matching version metadata and the expected
CA authenticate the reported identity; they do not establish measured code or
configuration provenance. A different KMS authority requires fresh CA,
configuration, governance and boot qualification; previously reviewed OS and
container bytes may be reusable, but existing prod5 acceptance cannot simply
be transferred to the new authority.

Production qualification remains rejected and new creation remains disabled. Any later
managed-policy acceptance must stay a finite synthetic pilot at `Open`, with
independent Covenant evidence checks. It is insufficient for stronger
production trust or provider-independent persistent-secret exclusivity.

## Prism comparison at a pinned revision

Prism `main` was checked at `4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec`.
Its inference verifier explicitly skips `key-custody`: no implemented verifier
appraises the dstack KMS custody chain. That documented skip is allowed in an
overall `verified` verdict; the label therefore does not establish exclusive
private-key custody. See the [skip and verdict rule](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/sdk/attest.mjs#L578-L601).

Prism's [confidential inference path](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/inference/README.md#L205-L212)
relays to Phala-hosted inference; this is distinct from provisioning and
closing a renter's attested node lease. Its [node lease code](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/node/prismd/src/attestation.rs#L184-L245)
binds a quote to a challenge, lease, node and guest SSH key. Code presence alone
does not demonstrate an accepted deployed lifecycle. The actual
[verification ceiling is `Confidential`](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/crates/protocol/src/lib.rs#L145-L167);
older prose describing a lower ceiling is not an activation record. An empty
public rental-offer list likewise says nothing about the separate inference
service's availability.

A repository-wide source search at that revision found no guest KMS contract
or server-policy implementation under `DstackApp`, `KmsAuth`, `kms_contract`,
`os_image_verification` or `image.verify`. This scoped finding does not identify
the upstream service's actual KMS policy. Prism's [AWS KMS signer](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/crates/chain/src/lib.rs#L731-L773)
signs chain transactions; it does not establish dstack guest key-release policy.

Keep Covenant's KMS qualification separate from launch, channel and receipt
verification. End-to-end encryption reduces relay plaintext exposure but does
not prove exclusive private-key custody. Prism supplies no evidence that
resolves the observed prod5 OS-verification gap or justifies raising `Open`.

Prism's [SNP guest bootstrap](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/node/prismd/assets/workspace-bootstrap.sh#L45-L121)
generates the SSH host key in guest runtime storage and binds its public key
to the lease challenge. Its [workspace client](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/sdk/workspace.mjs#L1-L40)
keeps snapshot encryption keys with the buyer. These are useful limits on
provider-held persistent secrets, not independent proof of guest key custody.
Its SNP [runtime configuration](https://github.com/prismnetwork-tech/prism/blob/4e32eb5eb1398fd9d11dcd29aa5a96e3f2f646ec/node/prismd/src/runtime.rs#L464-L489)
also puts `/workspace` in tmpfs, reducing restored plaintext written to disk.

Covenant's hardened guest generates the SSH private key in `/run` tmpfs and
keeps workspace encryption keys local to the buyer. Workspace plaintext and
the public host-key/status sidecar now use bounded `tmpfs,noswap` mounts.
The measured wrapper explicitly requests zero swap, and actual guest swap,
container cgroup limits, mount options and core-dump policy are checked before
key generation and continuously while access is open. A new versioned shared
volume prevents an update from reusing the historical disk-backed sidecar.
The [current-image live diagnostic](../../docs/compute-memory-diagnostic-evidence.md)
verified those controls on the existing CVM after fresh TDX verification.
That recorded runtime behavior remains separate from KMS qualification.

The collector replay journal and underlying Docker/containerd state remain
disk-backed. KMS compromise combined with backing-storage access can therefore
still affect runtime integrity; a fresh quote does not independently
authenticate every mutable file it reads. Memory-backed workspace hardening
does not qualify the KMS root or erase historical key exposure. These are
residual trust boundaries, not evidence of an observed compromise.
See dstack's [persistent container storage](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/os/common/rootfs/dstack-prepare.sh#L286-L303)
and [swap setup](https://github.com/Dstack-TEE/dstack/blob/7f143148cc49f4a09799958835d310e050137cb3/dstack/dstack-util/src/system_setup.rs#L2499-L2573).

## Isolated no-KMS hosting experiment

The [2026-10-20 no-KMS test](../../docs/compute-keyless-phala-feasibility.md)
created one separate Phala CVM with an exact `key_provider:none` wrapper.
Initial provider state and serial logs showed successful startup with temporary
keys. Direct networking was refused for the workspace, and updating/restarting
the guest failed to open its existing encrypted disk after key regeneration.
No signed quote or buyer connection was verified. The test resource was deleted,
with exact-UUID and workspace-list absence confirmed.

This is evidence for exploring fresh-VM/fresh-disk workers with an independent
connection path; it is not KMS qualification or a production-trust alternative.
The existing managed-KMS gate and production verification remain unchanged.

## Separate ephemeral profile

The subsequent [outbound acceptance test](../../docs/compute-ephemeral-phala-acceptance.md)
implements a distinct single-boot profile: fresh VM and encrypted disk per
lease, `key_provider:none`, no sealed environment or Phala gateway, a guarded
memory-backed workspace, and an outbound connection with a quote-bound SSH key.
It does not change the rejected managed-KMS qualification above or assume that
Phala changed its key-release policy.

The explicit verifier authenticates the complete pinned production launch
tuple and all runtime measurements, requires the `none`/empty provider and
encrypted ext4 storage events, and rejects the KMS-only OS-image event. The
pinned kernel/initrd and dm-verity root remain bound by the launch tuple. A
fresh quote and strict SSH handshake have passed with current Intel collateral.
The acceptance report records separate isolation, expiry and cleanup results.

This establishes an alternative for bounded ephemeral execution without a
managed-KMS secret-release dependency. It does not qualify persistent secrets
or change the rejected managed-KMS policy. The served trust ceiling remains
`Open`; native creation is disabled by default and requires an explicit bounded
operator allowance.

The [two subsequent native paid attempts](../../docs/compute-native-phala-acceptance.md)
used the deployed coordinator admission and signed relay. Independent guest and
buyer verification and native transfer passed, but deletion confirmation exceeded
each fixed gate: 60 seconds for the first attempt and 300 seconds for the
October 21 retry, whose absence confirmation was 18.926 seconds late. Both VMs
were later confirmed absent and both escrow returns finalized at zero Compute
charge. These remain failed paid qualifications, not managed-KMS acceptance.
Coordinator source `6598fe15` remains deployed; the operator is offline with
both one-use allowances consumed. The subsequent poll-only deletion repair is
local and not deployed. No third attempt or standing capacity is authorized;
successful separately approved paid acceptance is required before release.
