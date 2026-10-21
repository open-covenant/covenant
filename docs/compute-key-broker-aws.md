# Covenant application key broker: AWS foundation

Status: **offline candidate, not deployed or production qualified**. This package
keeps Phala workers in their existing fresh-VM `key_provider:none` profile and
places application-key custody in a separately controlled AWS KMS/Nitro boundary.
It does not replace dstack boot-disk KMS. No AWS resources, keys, grants, IAM roles,
paid tests or account changes are created by the included Python programs.

The broker must validate Phala TDX evidence itself. AWS KMS validates the Nitro
broker's recipient attestation; it does not accept arbitrary Phala TDX quotes.
The approved broker then performs tenant/owner/lease/workload authorization before
returning an application key encrypted to the attested worker recipient. See
[AWS attested KMS calls](https://docs.aws.amazon.com/kms/latest/developerguide/cryptographic-attestation.html).

## What is implemented

`deploy/compute-key-broker/aws/render.py` generates a CloudFormation foundation
and a standalone review copy of its KMS key policy using Python's standard
library. Account, region, role, parent image, network and explicit `policy_phase`
inputs are required; `config.required.json` deliberately contains only null values
and cannot render. The initial `provision_only` phase requires `approved_pcr0` and
`eif_sha256` to remain null and denies all decryption. The later
`signing_enrollment` and `service` phases require actual reviewed measurements.
Synthetic test values are confined to the test fixture. The renderer rejects
unknown phases, zero/debug PCR0, repeated-character placeholder hashes, example
account IDs, role overlap, unknown fields and missing network inputs. A
syntactically valid hash is not evidence that the EIF is correct: independently
review its actual build measurement and SHA-256 before authorizing decryption.

The generated foundation contains:

- One symmetric, AWS-origin, single-region wrapping key with automatic rotation.
- Distinct administrator, ciphertext-provisioning and EC2 parent identities.
  Administrator and provisioner roles must already exist. The parent role is new.
- A retained, private, versioned ciphertext bucket with Object Lock capability,
  and a retained DynamoDB table with point-in-time recovery and deletion
  protection. Object Lock has **no default retention period**; publication must
  set reviewed per-object retention before claiming immutable records.
- A private KMS interface endpoint across two or three availability zones and
  narrowly scoped S3/DynamoDB gateway endpoints. The parent security group permits
  HTTPS only to these endpoints; it has no public address, SSH ingress or NAT.
- An enclave-enabled EC2 launch template with IMDSv2 required and an encrypted
  root volume. Its Auto Scaling group has minimum, desired **and maximum zero**.
  There is no user-data installer, runtime activation flag or production default.

The parent IAM role can fetch ciphertext versions and ledger records, and request
attested `Decrypt`. It cannot write policies, ciphertext records, grants or key
material. A separately reviewed publisher and admission authority are still
required. The table does not automatically make policy versions authenticated or
prevent rollback; runtime verification and authoritative version advancement must
be implemented and qualified before using it as a source of authorization.

## Key policy and provisioning

The phases avoid a bootstrap cycle: the final EIF embeds the real root ARN and
wrapped signing seed, which cannot exist before the root is created. Keep the
configured `purpose` equal to the reviewed application purpose in every phase;
the reserved signing purpose is selected separately by the code.

| `policy_phase` | Required measurements | Permitted key use |
| --- | --- | --- |
| `provision_only` | Both null; no measurement tags are emitted | Separate provisioner may generate ciphertext-only application keys and signing seed; unconditional `Decrypt` deny for every principal, including an attested parent |
| `signing_enrollment` | Actual enrollment EIF PCR0 and SHA-256 | Only the reserved signing context can be provisioned or decrypted; application contexts are explicitly denied |
| `service` | Actual final service EIF PCR0 and SHA-256 | Configured application purpose plus the reserved signing context; only the final approved enclave may decrypt |

Every phase retains the same CloudFormation logical resource identities and keeps
ASG minimum, desired and maximum at zero. A phase change is a separately reviewed
policy update; it does not launch a parent or establish production readiness.

When decryption is enabled, it requires the configured parent role, exact nonzero EIF PCR0,
role-derived PCR3, the private VPC and a complete seven-field encryption context.
Missing/wrong attestation and alternate principals have explicit denies, so a
later identity policy cannot simply add ordinary decryption permission. PCR3 is
SHA-384 of 48 zero bytes followed by the parent IAM role ARN. Debug enclaves have
zero PCRs and fail the policy. [AWS measurement documentation](https://docs.aws.amazon.com/enclaves/latest/user/set-up-attestation.html)

The exact context is:

| Key | Source |
| --- | --- |
| `covenant:protocol` | Fixed `application-key-v1` |
| `covenant:broker_id` | Reviewed deployment identity |
| `covenant:purpose` | Reviewed application purpose or reserved `broker-response-signing` |
| `covenant:tenant_id` | Trusted key policy |
| `covenant:key_id` | Trusted key policy |
| `covenant:key_version` | Trusted key policy, decimal positive integer at most 2^53 - 1 |
| `covenant:owner_pubkey_b58` | Trusted key policy, owner identity |

The policy rejects missing, empty, extra or wrong namespace fields. The runtime
must enforce the richer tenant/key/version/owner semantics; IAM string conditions
are not a replacement. These values are authenticated to KMS ciphertext and are
**not secrets**: AWS logs encryption context. Do not put customer secrets or
personal data in IDs. [AWS encryption context](https://docs.aws.amazon.com/kms/latest/developerguide/encrypt_context.html)

`provision_request.py` prepares a `GenerateDataKeyWithoutPlaintext` request for
one 32-byte application key, using the exact context and an immutable key ARN.
Use `--key-kind response-signing` explicitly for the reserved signing seed;
`application` is the default and is rejected during `signing_enrollment`.
Only the separate provisioning role can perform this operation. It returns
ciphertext without exposing the plaintext to the provisioner. The broker role
can only unwrap it through attested decryption. This is an explicit choice to
create a **new managed key**, not an automatic migration of buyer-owned keys.
Record an intent and preserve the first response before publication; this KMS
operation is not idempotent, so an unknown response must not cause a silent retry
or replacement of the existing key/version.
[AWS ciphertext-only data-key API](https://docs.aws.amazon.com/kms/latest/APIReference/API_GenerateDataKeyWithoutPlaintext.html)

The broker response-signing seed has the same seven-field context, with
`tenant_id=broker_id`, `key_id=broker-response-signing`, `key_version=1` and
`purpose=broker-response-signing`. Its `owner_pubkey_b58` is the already pinned
coordinator/enrollment authority, **not** the initially unknown derived signer
public key. Enrollment uses `policy_phase=signing_enrollment` with a separate
approved EIF measurement; that phase admits only the reserved signing context.
The enclave derives the public
key after attested unwrap and emits a fresh, challenge-bound Nitro attestation
binding the public key, ciphertext hash and full enrollment context. Independently
verify that evidence before pinning the public key in the production EIF; replace
the enrollment measurement with the final production measurement. This bootstrap
must pass hardware qualification; a parent-provided public key is not evidence.
Never add an ordinary Encrypt or plaintext key-generation escape hatch.

Explicit denies cover ordinary `GenerateDataKey`, `Encrypt`, `ReEncrypt*`,
key-pair generation, grants, imports and replication. Scheduling key deletion is
also denied. Existing grants must be checked independently when reviewing any
previously created key; use a new root for this design rather than pretending an
old key becomes uncompromised after a policy edit.

The administrator can **change the key policy**, including its denies. Account
owners can also change role trust or administrative controls. This remains an
explicit trust boundary: IAM separation is not a cryptographic multisignature.
Use a dedicated governed account, independently reviewed role assumptions and
policy changes, organizational controls where available, and alerts with retention
outside the broker administrator's control. The template neither creates nor
claims those organizational guarantees.

## Offline workflow

Keep actual account IDs, roles, inventory, key IDs and generated outputs in a
private directory outside the repository. Copy the required configuration there
and fill it with approved values. Do not use the unit-test fixture for deployment.

```sh
python3 -m unittest discover -s deploy/compute-key-broker/aws -p 'test_*.py' -v
python3 deploy/compute-key-broker/aws/render.py \
  --config /absolute/private/config.json \
  --output-dir /absolute/private/new-review-directory
cfn-lint /absolute/private/new-review-directory/foundation.template.json
python3 deploy/compute-key-broker/aws/preflight.py \
  --config /absolute/private/config.json \
  --inventory /absolute/private/read-only-inventory.json
python3 deploy/compute-key-broker/aws/provision_request.py \
  --config /absolute/private/config.json \
  --metadata /absolute/private/new-key-metadata.json \
  --output /absolute/private/new-key-request.json
```

Use this ordered sequence only after the relevant infrastructure and test costs
are authorized; the scripts above perform no deployment or KMS operation:

1. Review the real parent AMI, network and separate identities, then render and
   preflight `provision_only` with both enclave measurement fields null. Review
   and apply the initial same-account CloudFormation change set. Record the
   returned immutable KMS root ARN and stack/resource identities. Every decrypt
   remains explicitly denied; do not insert a temporary or example PCR.
2. Prepare the signing-seed request with `--key-kind response-signing`, reserved
   tenant/key/version and the pinned coordinator as owner. The separate
   provisioner executes the ciphertext-only request inside the approved VPC.
   Preserve its original ciphertext and context before any next step. Application
   ciphertext records can also be created in this phase under reviewed metadata.
   Never repeat an ambiguous generation result as if it were idempotent.
3. Build the enrollment-only EIF using that real root ARN and wrapped seed.
   Independently review its actual PCR0 and EIF SHA-256. Change only the intended
   policy/image metadata to `signing_enrollment` and those measurements; review a
   change set against the **same stack and same retained root** before applying.
   Verify live effective policy and grants. Approve a separately bounded parent
   test launch; the generated ASG remains zero. Application-key decrypt must fail.
4. Run fresh nonce-bound enrollment and independently verify the Nitro proof as
   described in the [enrollment procedure](../deploy/compute-key-broker/enrollment/README.md).
   Preserve the consumed challenge and verified public-key receipt. Only this
   verified public key may enter the final broker and guest policies.
5. Build the final service EIF with the real application ciphertexts, root ARN and
   verified response public key. Review its actual measurements and a same-stack
   `service` policy update. The final policy replaces the enrollment image pin;
   it does not retain both measurements. Verify rejection of the old image and
   all required live deny cases before any production activation. ASG limits
   remain zero until a separate reviewed activation change.

`preflight.py` is an **initial-create inventory check**: it deliberately rejects
an already existing parent role. Do not bypass that check or reuse it as proof
that a phase update is safe. Updates require independent inspection of the
original stack, retained root ARN, unchanged resource identities, role trust,
current effective policy/grants and exact change set; automated update inventory
reconciliation is not implemented here. Phase labels alone cannot verify the
history or provenance of a submitted configuration.

The regression suite checks policy structure and a deliberately limited local
condition model. It is **not** AWS IAM authorization simulation, proof of an
attestation, or a replacement for real denial tests. Unknown policy operators
make the test model fail rather than silently accepting them. `cfn-lint` validates
resource schemas, not the security of the deployed system.

`preflight.py` reads captured AWS JSON only. Its inventory schema is:
`caller` (`sts get-caller-identity`), `region`, `instance_types`
(`ec2 describe-instance-types`), `images` (`ec2 describe-images`), `subnets`,
`route_tables`, `prefix_lists` (`ec2 describe-prefix-lists`) and `roles` (an array
of `iam get-role` responses). Capture only the configured resources and retain
capture timestamps and caller details privately. The checker validates Nitro
support, matching AMI architecture, separate AZs, private subnet associations,
regional endpoint prefix lists and pre-existing role identities. Verify current
account-plan eligibility and quotas separately; account verification failures are
not an invitation to bypass verification or upgrade the plan automatically.

Before any approved foundation deployment, validate the template with AWS and
review the exact CloudFormation change set. The CloudFormation execution role
must be the configured key administrator and have separately reviewed stack
creation permissions; the key policy does not enable account-wide IAM delegation.
Do not bypass KMS's lockout safety check. Creation also requires the normal
named-IAM capability acknowledgment. Retained resources continue to exist and may
cost money after a stack deletion.

## Remaining activation work

1. Complete the ordered provisioning/enrollment/service workflow above. Build and
   review the immutable parent AMI and enclave images; pin actual EIF
   digest/PCR0, dependency versions and production launch settings. Never enable
   debug or console attachment. Confirm the chosen instance type is both
   enclave-capable and permitted by the account's current plan and quotas.
2. Qualify the private ingress and vsock transport. Source now includes the
   loopback-only parent JSON relay and bounded enclave entrypoint, but no live
   parent AMI, authenticated HTTPS ingress or installed KMS vsock proxy. The configured existing
   ingress security group represents a reviewed control-plane gateway/provisioner
   network; it may reach the private broker and KMS endpoint. This foundation does
   not create a public endpoint for Phala. The parent must forward KMS TLS bytes
   unchanged; KMS TLS terminates inside the enclave and must
   validate the real regional AWS hostname through the vsock proxy, with no
   certificate-verification bypass. The enclave's credentials/proxies and TDX
   collateral path must be reviewed together.
3. Implement authenticated durable policy publication, conditional monotonic
   version/revocation updates and the admission authority; wire those into the
   runtime. Parent-controlled storage alone must not authorize release. Keep
   lease freshness short and fail closed on unavailable authority, expiry or
   rollback. Retain audit events independently; CloudTrail management events alone
   are not a complete application key-release audit.
4. Run AWS policy validation/simulation where supported and actual KMS calls:
   ordinary/missing/zero/wrong PCR0, wrong PCR3/role, context alteration, cross-tenant
   substitution and alternate grants/reencryption must fail. A real approved
   enclave must return recipient-encrypted output and the host must not receive
   plaintext. Capture full effective policy/SCP/endpoint/grant state, not only the
   generated document.
5. Exercise two brokers in separate AZs, host/enclave death, full broker loss,
   ciphertext and policy restore, stale backup rejection, key rotation and future
   release revocation. KMS root loss is not solved by S3 backups. The current
   single-region root does not provide cross-region disaster recovery; that needs
   a separately reviewed replica policy/architecture. Never delete an old root
   while ciphertext still depends on it. Complete an independent security review
   and a Phala end-to-end acceptance before raising the ASG maximum or enabling
   production key release.

AWS documents private KMS endpoint routing and endpoint policies separately from
key authorization. [KMS VPC endpoints](https://docs.aws.amazon.com/kms/latest/developerguide/kms-vpc-endpoint.html)
Nitro's isolation and parent-proxy requirements do not remove dependency on AWS
hardware/services or Covenant's governance.
[AWS Nitro Enclaves](https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave.html)

## Cost review boundary

No resource has been priced or approved for creation by this package. The dormant
foundation still has a billable KMS key and one interface endpoint ENI per AZ;
storage, DynamoDB, API requests and logs add usage charges. At the published
$1/month initial KMS key rate, key storage is a small component; rotations can add
storage charges. [KMS pricing](https://aws.amazon.com/kms/pricing/)

Price the approved region and instance type before proposing a dollar allowance:
`broker count × EC2 hourly rate × hours`, encrypted EBS per parent, `AZ count ×
PrivateLink endpoint-hour rate × hours`, KMS key/API use, S3 versions/retention,
DynamoDB reads/writes/backups, audit logging, ingress and data transfer. Nitro has
no separate enclave surcharge, but the enclosing EC2 instances are billed.
[EC2 pricing](https://aws.amazon.com/ec2/pricing/on-demand/),
[PrivateLink pricing](https://aws.amazon.com/privatelink/pricing/).
Free credits are not a spend cap or proof that the selected instance is available
on the account plan. A production two-AZ service requires a separate cost approval
from a bounded hardware acceptance test.
