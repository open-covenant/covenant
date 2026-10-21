#!/usr/bin/env python3
"""One-shot application-key recipient; only public protocol messages use stdio.

Run in a separately measured container with no buyer processes and sole access
(to this role) to dstack.sock. Private RSA key material is never serialized.
This is a candidate, not a production-qualified guest image. Production
RuntimeGuard has no disable/test flags; dependency injection is test-only.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import stat
import struct
import sys
import time

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, padding, rsa

# Installed read-only beside the existing measured collector helpers. Never
# resolve modules from the buyer-writable /workspace directory.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "compute-attested-cpu"))
sys.path.insert(0, "/opt/covenant")
import collector
from memory_guard import RuntimeGuard, MIB, bounded_read, mounts_from, require_tmpfs

DOMAIN = b"covenant.compute.key-broker.report-data.v1\0"
RESPONSE_LABEL = b"covenant.compute.key-broker.key-response.v1:"
RESPONSE_SIGNATURE_DOMAIN = b"covenant.compute.key-broker.release-response.v1\0"
MAX_MESSAGE = 512 * 1024
KEY_FILENAME = ".covenant-app-key"
SOCKET_PATH = "/key-broker-io/recipient.sock"
KEY_POLICY_FIELDS = {"protocol_version", "broker_id", "tenant_id", "key_id",
                     "key_version", "purpose", "lease_id", "broker_response_pubkey_b58"}
CHALLENGE_FIELDS = {"protocol_version", "broker_id", "challenge_id", "nonce_hex",
                    "ttl_ms", "binding", "recipient_spki_der_base64"}
RESPONSE_FIELDS = {"protocol_version", "challenge_id", "challenge_sha256",
                   "key_id", "key_version", "ciphertext_base64", "algorithm"}


class ServiceRuntimeGuard(RuntimeGuard):
    def check(self, role):
        super().check(role)
        mounts = mounts_from(bounded_read(self.proc / "self/mountinfo", 512 * 1024))
        require_tmpfs(mounts, "/key-broker-io", MIB)


def key_policy(value):
    collector.exact_fields(value, KEY_POLICY_FIELDS)
    collector.integer(value["protocol_version"], 1, 1)
    collector.integer(value["key_version"], 1)
    collector.public_identity(value["broker_response_pubkey_b58"])
    for name in KEY_POLICY_FIELDS - {"protocol_version", "key_version", "broker_response_pubkey_b58"}:
        collector.require(type(value[name]) is str and
                          re.fullmatch(r"[A-Za-z0-9_.:/-]{1,128}", value[name]),
                          "invalid key policy identifier")
    return value


def digest(challenge):
    return hashlib.sha256(collector.canonical(challenge)).hexdigest()


def report_data(challenge):
    return hashlib.sha512(DOMAIN + collector.canonical(challenge)).digest()


def b64decode(value, maximum):
    collector.require(type(value) is str and len(value) <= maximum * 2,
                      "invalid encoded value")
    try:
        result = base64.b64decode(value, validate=True)
    except (ValueError, TypeError) as error:
        raise collector.Rejected("invalid encoded value") from error
    collector.require(len(result) <= maximum and base64.b64encode(result).decode() == value,
                      "noncanonical encoded value")
    return result


def base58_decode(value, length):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    collector.require(type(value) is str and 1 <= len(value) <= length * 2,
                      "invalid base58 value")
    number = 0
    for character in value:
        collector.require(character in alphabet, "invalid base58 value")
        number = number * 58 + alphabet.index(character)
    raw = b"\0" * (len(value) - len(value.lstrip("1")))
    raw += number.to_bytes((number.bit_length() + 7) // 8, "big")
    collector.require(len(raw) == length, "invalid base58 value length")
    return raw


class Recipient:
    def __init__(self, policy, lease, guest, *, guard=None, clock=None, monotonic=None):
        self.policy = key_policy(policy)
        self.clock = clock or (lambda: int(time.time() * 1000))
        self.monotonic = monotonic or time.monotonic
        self.lease = collector.load_policy(lease, self.clock())
        collector.require(self.lease.get("profile") == "phala_ephemeral_dstack061",
                          "ephemeral lease required")
        collector.require(self.policy["lease_id"] == self.lease["job_id"],
                          "key lease ID must match the durable job")
        self.guest = guest
        self.guard = guard if guard is not None else RuntimeGuard()
        self.deadline = self.monotonic() + (
            self.lease["lease_expires_at_ms"] - self.clock()) / 1000
        self.challenge_deadline = None
        self.challenge = None
        self.spent = False
        self.check()
        self._key = rsa.generate_private_key(public_exponent=65537, key_size=3072)
        self.public = base64.b64encode(self._key.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )).decode("ascii")
        self.info = self.identity()
        self.binding = {name: self.policy[name] for name in
                        ("tenant_id", "key_id", "key_version", "purpose", "lease_id")}
        self.binding.update({
            "owner_pubkey_b58": self.lease["buyer_pubkey_b58"],
            "job_id": self.lease["job_id"],
            "lease_generation": self.lease["lease_generation"],
            "operator_pubkey_b58": self.lease["operator_pubkey_b58"],
            "workload_sha256": self.info["compose_hash"],
            "phala_app_id": self.info["app_id"],
            "phala_instance_id": self.info["instance_id"],
        })

    def check(self):
        collector.require(self.clock() < self.lease["lease_expires_at_ms"] and
                          self.monotonic() < self.deadline, "expired lease")
        if self.challenge_deadline is not None:
            collector.require(self.monotonic() < self.challenge_deadline,
                              "expired key challenge")
        # Dedicated isolated role uses the SSH-sized cgroup and complete memory
        # mounts, but never runs SSH or a buyer command in this container.
        self.guard.check("ssh")

    def identity(self):
        self.check()
        info = self.guest.request("Info")
        collector.require(info.get("os_image_hash") == collector.OS_IMAGE_HASH,
                          "unexpected guest OS")
        collector.hex_value(info.get("compose_hash"), 32)
        for field in ("app_id", "instance_id"):
            collector.hex_value(info.get(field), 20)
        config = info.get("vm_config")
        collector.require(type(config) is str, "missing VM configuration")
        config = collector.strict_json(config)
        collector.require(type(config) is dict and
                          config.get("key_provider") == {"name": "none", "id": ""},
                          "unexpected key provider")
        self.check()
        return {name: info[name] for name in
                ("compose_hash", "app_id", "instance_id", "os_image_hash", "vm_config")}

    def hello(self):
        self.check()
        return {"protocol_version": 1, "recipient_spki_der_base64": self.public,
                "binding": collector.strict_json(collector.canonical(self.binding))}

    def collect(self, challenge):
        collector.require(self.challenge is None and not self.spent,
                          "recipient already used")
        collector.exact_fields(challenge, CHALLENGE_FIELDS)
        collector.integer(challenge["protocol_version"], 1, 1)
        collector.uuid_value(challenge["challenge_id"])
        collector.hex_value(challenge["nonce_hex"], 32)
        ttl = collector.integer(challenge["ttl_ms"], 1, 60_000)
        collector.require(challenge["broker_id"] == self.policy["broker_id"],
                          "wrong key broker")
        # Compare canonical bytes as well as schema types: bool cannot masquerade
        # as integer 1, and unknown binding fields cannot ride into a quote.
        collector.require(collector.canonical(challenge["binding"]) ==
                          collector.canonical(self.binding), "wrong key or lease binding")
        collector.require(challenge["recipient_spki_der_base64"] == self.public,
                          "recipient key is not owned by this process")
        self.check()
        self.challenge = collector.strict_json(collector.canonical(challenge))
        self.challenge_deadline = self.monotonic() + ttl / 1000
        # Mark the challenge before external I/O. Any RPC error consumes this
        # one-shot recipient; the executable exits instead of retrying.
        self.spent = True
        collector.require(self.identity() == self.info, "guest identity changed")
        raw_report = report_data(self.challenge)
        quote = self.guest.request("GetQuote", {"report_data": raw_report.hex()})
        collector.require(quote.get("report_data") == raw_report.hex(),
                          "guest report-data mismatch")
        encoded = quote.get("quote")
        collector.require(type(encoded) is str and
                          re.fullmatch(r"[0-9a-fA-F]{2,49152}", encoded) and
                          len(encoded) % 2 == 0, "invalid quote encoding")
        collector.require(quote.get("vm_config") == self.info["vm_config"],
                          "guest VM configuration changed")
        events = collector.strict_json(quote.get("event_log", ""))
        collector.require(type(events) is list and 0 < len(events) <= 256,
                          "invalid event log")
        collector.require(self.identity() == self.info, "guest identity changed")
        self.check()
        # These are raw evidence, not a local authenticity verdict. The broker
        # obtains Intel-signed collateral and performs independent DCAP+RTMR.
        return {"challenge": self.challenge, "evidence": {
            "quote_base64": base64.b64encode(bytes.fromhex(encoded)).decode(),
            "events": events}}

    def decrypt(self, signed):
        collector.require(self.challenge is not None and self._key is not None,
                          "no pending key challenge")
        self.check()
        collector.exact_fields(signed, {"response", "broker_pubkey_b58", "signature_b58"})
        collector.require(signed["broker_pubkey_b58"] == self.policy["broker_response_pubkey_b58"],
                          "wrong broker response signer")
        response = signed["response"]
        collector.exact_fields(response, RESPONSE_FIELDS)
        collector.integer(response["protocol_version"], 1, 1)
        collector.integer(response["key_version"], 1)
        expected = {"protocol_version": 1,
                    "challenge_id": self.challenge["challenge_id"],
                    "challenge_sha256": digest(self.challenge),
                    "key_id": self.binding["key_id"],
                    "key_version": self.binding["key_version"],
                    "algorithm": "RSA_OAEP_SHA256"}
        collector.require(all(response[name] == value for name, value in expected.items()),
                          "key response belongs to another request")
        # OAEP is encryption, not sender authentication. Verify the independently
        # pinned broker signature before any private-key operation; an untrusted
        # parent/relay must not replace the application key with chosen bytes.
        try:
            ed25519.Ed25519PublicKey.from_public_bytes(base58_decode(
                signed["broker_pubkey_b58"], 32)).verify(
                    base58_decode(signed["signature_b58"], 64),
                    RESPONSE_SIGNATURE_DOMAIN + collector.canonical(response))
        except InvalidSignature as error:
            raise collector.Rejected("invalid broker response signature") from error
        ciphertext = b64decode(response["ciphertext_base64"], 384)
        collector.require(len(ciphertext) == 384, "wrong ciphertext length")
        key, self._key = self._key, None
        try:
            plaintext = key.decrypt(ciphertext, padding.OAEP(
                mgf=padding.MGF1(hashes.SHA256()), algorithm=hashes.SHA256(),
                label=RESPONSE_LABEL + digest(self.challenge).encode("ascii")))
        except ValueError as error:
            raise collector.Rejected("key response decryption failed") from error
        collector.require(len(plaintext) == 32, "unexpected application key length")
        self.check()
        return plaintext


def deliver(plaintext, *, workspace="/workspace", uid=10001, gid=10001):
    """Only the production caller uses the fixed path and owner; test seams
    never become CLI options. Directory-relative O_EXCL/O_NOFOLLOW prevents
    symlink/overwrite attacks from the buyer-writable workspace.
    """
    collector.require(len(plaintext) == 32, "unexpected application key length")
    directory = os.open(workspace, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW |
                        os.O_CLOEXEC)
    descriptor = None
    created = False
    try:
        meta = os.fstat(directory)
        collector.require(stat.S_ISDIR(meta.st_mode) and meta.st_uid == uid and
                          meta.st_gid == gid and stat.S_IMODE(meta.st_mode) == 0o700,
                          "unsafe key workspace")
        descriptor = os.open(KEY_FILENAME, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                             os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=directory)
        created = True
        os.fchmod(descriptor, 0o600)
        os.fchown(descriptor, uid, gid)
        with os.fdopen(descriptor, "wb") as output:
            descriptor = None
            output.write(plaintext)
            output.flush()
        return {"protocol_version": 1, "status": "delivered", "path": "/workspace/" + KEY_FILENAME}
    except BaseException:
        if created:
            os.unlink(KEY_FILENAME, dir_fd=directory)
        raise
    finally:
        if descriptor is not None:
            os.close(descriptor)
        os.close(directory)


def read_message(source=None):
    source = source if source is not None else sys.stdin.buffer
    raw = source.readline(MAX_MESSAGE + 1)
    collector.require(raw.endswith(b"\n") and len(raw) <= MAX_MESSAGE,
                      "missing or oversized protocol message")
    return collector.strict_json(raw)


def emit(value, target=None):
    target = target if target is not None else sys.stdout.buffer
    target.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
    target.flush()


def exchange(recipient, source, target):
    emit(recipient.hello(), target)
    bundle = recipient.collect(read_message(source))
    signal.setitimer(signal.ITIMER_REAL,
                     max(0.001, min(recipient.deadline, recipient.challenge_deadline) - time.monotonic()))
    emit(bundle, target)
    plaintext = recipient.decrypt(read_message(source))
    recipient.check()
    receipt = deliver(plaintext)
    del plaintext
    emit(receipt, target)


def serve(recipient):
    # One connection/recipient, with a caller confined to the existing buyer
    # UID in the attested SSH container. No restart, reconnect, or quote oracle.
    recipient.check()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(SOCKET_PATH)  # Refuse a pre-existing path; never unlink it.
        os.chmod(SOCKET_PATH, 0o600)
        listener.listen(1)
        listener.settimeout(max(0.001, recipient.deadline - time.monotonic()))
        with listener.accept()[0] as connection:
            collector.require(hasattr(socket, "SO_PEERCRED"), "Unix peer credentials unavailable")
            _, uid, gid = struct.unpack("3i", connection.getsockopt(
                socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
            collector.require(uid == 10001 and gid == 10001, "wrong key recipient peer")
            connection.settimeout(max(0.001, recipient.deadline - time.monotonic()))
            with connection.makefile("rwb") as stream:
                exchange(recipient, stream, stream)


def main():
    collector.require(sys.argv[1:] in ([], ["serve"]), "invalid recipient mode")
    collector.require(os.geteuid() == 10001 and os.getegid() == 10001,
                      "recipient requires dedicated unprivileged container")
    os.umask(0o077)
    policy = key_policy(collector.strict_json(b64decode(
        os.environ.get("COVENANT_KEY_POLICY_B64", ""), 8192)))
    lease = collector.environment_policy()
    service = sys.argv[1:] == ["serve"]
    recipient = Recipient(policy, lease, collector.Dstack(),
                          guard=ServiceRuntimeGuard() if service else RuntimeGuard())
    # Interrupt blocked input/RPC at the immutable lease deadline. A fresh
    # challenge further narrows this timer; no caller-controlled extension.
    def expired(_signum, _frame):
        raise collector.Rejected("recipient deadline expired")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, max(0.001, recipient.deadline - time.monotonic()))
    if service:
        serve(recipient)
    else:
        exchange(recipient, sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # Never echo request fields, ciphertext, secrets, or exception arguments.
        print('{"event":"key_recipient_failed"}', file=sys.stderr)
        raise SystemExit(1)
