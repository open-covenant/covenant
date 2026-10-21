# Independent Nitro signing-key enrollment

This offline verifier checks a real Nitro COSE attestation against an independently
prepared challenge, immutable KMS key ARN, ciphertext hash, enrollment EIF PCR0
and parent-role PCR3. It is candidate security code: synthetic tests pass, but a
real Nitro enrollment and independent security review remain required.

Create the wrapping root first using the foundation's `provision_only` phase,
which requires null enclave measurements and denies every decrypt. Generate the
signing seed with the separate provisioner using `--key-kind response-signing`
and preserve its ciphertext. Build the enrollment EIF with that actual root ARN
and wrapped seed, then independently review its measurements before a same-stack
`signing_enrollment` policy update. That phase accepts only the reserved signing
context, never application-key decryption. After verified enrollment, build and
review the final EIF and replace the enrollment measurement using `service`.
All generated ASG capacities remain zero. See the full
[ordered foundation workflow](../../../docs/compute-key-broker-aws.md#offline-workflow);
no temporary PCR or ordinary decrypt permission is needed to bootstrap.

The bundled `aws-nitro-root-g1.pem` was downloaded from AWS's published
[root archive](https://aws-nitro-enclaves.amazonaws.com/AWS_NitroEnclaves_Root-G1.zip).
The verifier checks its DER SHA-256 fingerprint on every run:
`641a0321a3e244efe456463195d606317ed7cdcc3c1756e09893f3c68f79bb5b`.
The source of this pin is the official
[AWS root-verification documentation](https://docs.aws.amazon.com/enclaves/latest/user/verify-root.html).
No CLI option accepts a different root, parent-supplied measurement or clock.

Use Python 3.10 or newer and install `requirements.txt` in an isolated environment
with `pip install --require-hashes --only-binary=:all: -r requirements.txt`. The
pins are cryptography 50.0.2, pyOpenSSL 26.4.0 and cbor2 6.1.5, with every
transitive dependency version/hash fixed. These current releases were checked
against their official release notes on 2026-10-21; package review is not a
security audit. PyOpenSSL
performs X.509 path validation with strict constraints against **only** the pinned
root; PyCA verifies the COSE ES384 signature. Certificate validity uses the local
trusted verifier clock. The verifier requires a non-CA P-384 leaf with digital
signature usage, all certificates currently valid, the expected full chain,
nonzero matching PCR0/PCR3, a timestamp no earlier than the challenge and at most
120 seconds old (five seconds future skew), and exact nonce/public-key/user-data
bindings. Certificate revocation is not fetched; this follows AWS's documented
Nitro verification model and must be considered in the threat assessment.

A bounded definite-length CBOR parser rejects duplicate maps, trailing bytes,
unrecognized tags, indefinite lengths, deep nesting and types outside the Nitro
schema. `cbor2` is used only to encode COSE's signature structure. The verifier's
restricted ASCII enrollment schema with integer protocol version 1 makes its
canonical JSON identical to JCS for these objects. The enclave's attested
`public_key` field carries the raw 32-byte Ed25519 response key by this protocol.

The expected manifest contains exactly:

```json
{
  "protocol_version": 1,
  "broker_id": "INDEPENDENTLY_REVIEWED",
  "coordinator_pubkey_b58": "INDEPENDENTLY_REVIEWED",
  "kms_key_arn": "INDEPENDENTLY_REVIEWED_IMMUTABLE_KEY_ARN",
  "wrapped_key_sha256": "INDEPENDENTLY_COMPUTED_CIPHERTEXT_HASH",
  "approved_pcr0": "INDEPENDENTLY_REVIEWED_ENROLLMENT_EIF_PCR0",
  "approved_pcr3": "INDEPENDENTLY_COMPUTED_PARENT_ROLE_PCR3"
}
```

These placeholders intentionally fail validation. Never copy expected values from
the proof being verified. Keep manifests, challenges and receipts outside the
repository. The new challenge script adds a random 32-byte nonce and local creation
time; issue it only after the enrollment enclave is ready.

```sh
python new_challenge.py --manifest /private/reviewed-manifest.json \
  --output /private/new-challenge.json
# Give its nonce to the separately qualified enclave's enroll-signing operation.
python verify.py --proof /private/enrollment-proof.json \
  --challenge /private/new-challenge.json --receipt /private/new-receipt.json
python -m unittest discover -s . -p 'test_*.py' -v
```

A successful CLI verification atomically creates a `.consumed` marker adjacent to
the challenge before writing the receipt. Reusing that challenge is rejected even
if another output path is provided. Preserve markers and uncertain outcomes. Do
not copy/recreate an old challenge or change its time to bypass expiry. Receipt
write failure after consumption requires a fresh challenge and proof.

Only after independently verifying enrollment may the derived public key be
pinned in the production broker and guest policy. Replace the temporary enrollment
measurement in KMS with the final production measurement; do not leave both images
approved. A valid enrollment proves measured execution and key
binding, not the correctness of the application, AWS-account governance,
persistent policy recovery, lease admission or production readiness.
