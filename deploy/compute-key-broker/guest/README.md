# Candidate Phala application-key recipient

This directory adds an isolated recipient for **new, separately qualified**
application-key leases. The opt-in uses a new measured recipe and pinned
recipient image. Existing no-KMS leases, buyer-owned snapshot keys and the
production trust ceiling remain unchanged.

The process generates an RSA-3072 key in memory. Its measured key policy pins
broker identity and response-signing public key, tenant, key/version, purpose
and lease ID. The existing immutable lease policy supplies buyer, job,
operator, generation and expiry; dstack Info supplies consistency hints for
Compose/app/instance identity. Those hints must be authenticated independently
by the broker's Intel DCAP and RTMR verification, never trusted by themselves.

The dedicated recipient container has the dstack socket; the buyer's SSH
container does not. No buyer processes or writable code/policy enter the
recipient container. The inherited collector remains isolated and supports
only its existing, separately domain-separated SSH admission protocol. A
buyer-uploaded script with unrestricted quote access is not this design.

## Protocol

Native opt-in leases run `recipient.py serve` in the dedicated UID/GID 10001
container. The sole protocol socket is `/key-broker-io/recipient.sock`, on a
whole 1 MiB `noswap` tmpfs shared only with SSH. A single buyer UID/GID 10001
peer is admitted via kernel Unix peer credentials. The buyer runs the fixed
`ssh_bridge.py` with Python isolated mode over the already attested SSH channel; the bridge accesses no
quote API or private keys. Direct stdio mode remains available with no
arguments for isolated qualification. Its three JSON outputs are public:

1. `{protocol_version, recipient_spki_der_base64, binding}`.
2. After receiving the broker challenge on stdin: `{challenge, evidence}`,
   with raw quote base64 and native event entries. The controller obtains
   Intel-signed collateral and buyer/coordinator authorization separately.
3. After receiving a signed release: a delivery status and fixed memory path.

Input challenge fields match the broker's `BrokerChallenge`. The RSA SPKI
recipient must equal the process-owned key, every binding must equal its
measured policy/runtime identity, and TTL is at most 60 seconds. Report data
is exactly 64 bytes: `SHA512("covenant.compute.key-broker.report-data.v1\0" ||
JCS(challenge))`. The frozen dstack `/GetQuote` call receives these bytes as
hex, without an additional SDK hash. ASCII strings and safe nonnegative JSON
integers form the supported JCS subset; duplicate keys/floats are rejected.
`challenge-vector.json` is a public cross-language binding vector.

The release must be `SignedReleaseResponse` with exact fields `response`,
`broker_pubkey_b58`, `signature_b58`. The measured guest policy independently
pins the broker's Ed25519 public key. Its signature covers
`"covenant.compute.key-broker.release-response.v1\0" || JCS(response)`.
An untrusted relay/parent cannot substitute an encrypted key just because it
knows the RSA public key. The response also must match the pending challenge
hash, ID, key ID/version and RSA OAEP SHA-256 algorithm. OAEP's MGF1 uses
SHA-256; its ASCII label is
`covenant.compute.key-broker.key-response.v1:` followed by lowercase
`SHA256(JCS(challenge))` hex.

Only a 32-byte plaintext is accepted. It is written once to
`/workspace/.covenant-app-key`, mode `0600`, owner/group `10001`, using
`O_EXCL` and `O_NOFOLLOW` relative to the checked workspace directory. The
process never serializes its RSA private key and never logs plaintext. Python
and its crypto library do not promise complete zeroization of all temporary
allocations; confidentiality relies on the checked non-swappable memory
runtime and process/VM lifecycle. The one-shot process exits after delivery.
The existing lease shutdown controls workspace access; delivery does not
promise to retract a key already read by an authorized workload.

The executable enforces `RuntimeGuard` before key generation, quote and
release: swap disabled, cgroup swap zero, core dumps disabled, exact memory
limits and required whole `noswap` tmpfs mounts. An immutable lease timer
bounds blocking stdio; a monotonic challenge timer narrows it. Failure exits
without a retry or a new recipient. A process restart creates a new key, so an
old challenge cannot authorize that new recipient.

## Policy and image

`COVENANT_KEY_POLICY_B64` encodes an exact JSON object:

```json
{
  "protocol_version": 1,
  "broker_id": "approved-broker-id",
  "broker_response_pubkey_b58": "INDEPENDENTLY_QUALIFIED_BROKER_PUBLIC_KEY",
  "tenant_id": "tenant-id",
  "key_id": "application-key-id",
  "key_version": 1,
  "purpose": "workspace",
  "lease_id": "ddff04e5-aeb5-44ee-a137-4164c24d6597"
}
```

`lease_id` must equal the canonical job UUID in the existing
`COVENANT_LEASE_POLICY_B64`, which is supplied unchanged. These public
policy values belong in the exact measured Compose; do not pass plaintext
keys or broker signing seeds as environment variables. The pinned broker
public key requires an independently verified bootstrap procedure. Accepting
a key returned by an untrusted parent is not bootstrap verification.

`Dockerfile` extends the existing guest by exact digest and adds only this
recipient and fully hashed Python dependencies. `compose.candidate.template.yaml`
uses explicit immutable SSH/collector and recipient image digests. The
isolated, networkless recipient shares the memory workspace and public IPC
tmpfs with SSH, while retaining sole custody of its process key. The optional
`ApplicationKeyRecipient` field in the buyer-signed Phala spec selects this
compiled recipe; omitting it preserves the original wire format and Compose
bytes. The broker independently rebuilds the manifest from the signed job and
its pinned images/policy before accepting its workload hash. This source
integration still requires image build, live qualification and explicit
production policy admission. No currently deployed image pin is replaced.

## Offline validation

```sh
python3 -m venv /tmp/covenant-key-recipient-tests
/tmp/covenant-key-recipient-tests/bin/pip install --require-hashes --only-binary=:all: -r deploy/compute-key-broker/guest/requirements.txt
/tmp/covenant-key-recipient-tests/bin/python -m unittest discover -s deploy/compute-key-broker/guest -p 'test_*.py' -v
```

Tests use real RSA/Ed25519/OAEP and **mock TDX/runtime evidence**. They verify
recipient ownership, context binding, hostile response substitution, expiry,
fail-closed runtime checks and safe file delivery. They do not claim a live
TDX quote, Nitro execution, KMS release, image qualification or production
readiness. Build and container runtime smoke tests require Linux Docker with
the production memory guard requirements; no guard bypass is provided.
