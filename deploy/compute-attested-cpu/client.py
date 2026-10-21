#!/usr/bin/env python3
"""Bounded local Phala diagnostic client. Every collected response is unverified."""
import argparse
import contextlib
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import select
import signal
import socket
import ssl
import stat
import sys
import time

import collector as c

GATEWAY = "dstack-pha-prod5.phala.network"
MAX_BODY = 1024 * 1024
HTTP_SECONDS = 30


class ClientError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ClientError(message)


def destination(instance_id, port):
    require(type(instance_id) is str and re.fullmatch(r"[0-9a-f]{40}", instance_id),
            "instance ID must be exactly 40 lowercase hexadecimal characters")
    require(port in (1022, 18090), "unsupported diagnostic port")
    return f"{instance_id}-{port}.{GATEWAY}"


def private_read(path, maximum):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        details = os.fstat(descriptor)
        require(stat.S_ISREG(details.st_mode) and details.st_uid == os.getuid()
                and details.st_mode & 0o077 == 0, "input must be an owner-only regular file")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            value = source.read(maximum + 1)
        require(len(value) <= maximum, "input file too large")
        return value
    finally:
        os.close(descriptor)


def read_token(path):
    raw = private_read(path, 65)
    if raw.endswith(b"\n"):
        raw = raw[:-1]
    require(re.fullmatch(rb"[0-9a-f]{64}", raw), "invalid collector bearer file")
    return raw.decode("ascii")


@contextlib.contextmanager
def output_files(path):
    """Reserve both files exclusively before issuing a potentially consumed POST."""
    path = Path(path)
    parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    body_fd = metadata_fd = None
    try:
        details = os.fstat(parent)
        require(details.st_uid == os.getuid() and details.st_mode & 0o077 == 0,
                "output directory must be owner-only")
        body_fd = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                          0o600, dir_fd=parent)
        try:
            metadata_fd = os.open(path.name + ".metadata.json",
                                  os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                  0o600, dir_fd=parent)
        except Exception:
            os.close(body_fd)
            body_fd = None
            os.unlink(path.name, dir_fd=parent)
            raise
        os.fsync(parent)
        with os.fdopen(body_fd, "wb", closefd=False) as body, os.fdopen(
            metadata_fd, "wb", closefd=False
        ) as metadata:
            yield body, metadata
            body.flush()
            metadata.flush()
            os.fsync(body_fd)
            os.fsync(metadata_fd)
        os.fsync(parent)
    finally:
        if body_fd is not None:
            os.close(body_fd)
        if metadata_fd is not None:
            os.close(metadata_fd)
        os.close(parent)


@contextlib.contextmanager
def deadline(seconds):
    """Wall-time bound covers DNS, TLS, slow headers and response streaming."""
    require(signal.getitimer(signal.ITIMER_REAL) == (0.0, 0.0), "another timer is active")
    previous = signal.getsignal(signal.SIGALRM)

    def expired(_signum, _frame):
        raise ClientError("diagnostic deadline exceeded")

    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def checked_challenge(path, instance_id, now_ms):
    raw = private_read(path, c.MAX_REQUEST)
    challenge = c.strict_json(raw)
    c.exact_fields(challenge, c.CHALLENGE_FIELDS)
    c.exact_fields(challenge["scope"], c.SCOPE_FIELDS)
    c.integer(challenge["schema_version"], 1, 1)
    c.uuid_value(challenge["challenge_id"])
    c.hex_value(challenge["nonce_hex"], 32)
    issued = c.integer(challenge["issued_at_ms"])
    expires = c.integer(challenge["expires_at_ms"])
    require(issued <= now_ms < expires <= issued + 120_000, "challenge is not fresh")
    scope = challenge["scope"]
    require(scope["host"] == destination(instance_id, 1022)
            and type(scope["port"]) is int and scope["port"] == 443
            and scope["username"] == "covenant"
            and scope["transport"] == {"kind": "tls"}, "challenge endpoint does not match instance")
    c.integer(scope["lease_expires_at_ms"], expires)
    c.canonical(challenge)
    return raw


def fetch(instance_id, bearer_file, output, challenge_file=None):
    host = destination(instance_id, 18090)
    token = read_token(bearer_file)
    now_ms = int(time.time() * 1000)
    body = checked_challenge(challenge_file, instance_id, now_ms) if challenge_file else None
    path = "/quote" if body is not None else "/identity"
    method = "POST" if body is not None else "GET"
    metadata = {
        "schema_version": 1, "verification_status": "unverified",
        "url": f"https://{host}{path}", "method": method,
        "request_body_sha256": hashlib.sha256(body).hexdigest() if body is not None else None,
        "started_at_ms": now_ms, "body_complete": False,
        "automatic_retries": 0,
    }
    captured = b""
    failure = None
    with output_files(output) as (saved_body, saved_metadata):
        connection = None
        try:
            with deadline(HTTP_SECONDS):
                context = ssl.create_default_context()
                context.minimum_version = ssl.TLSVersion.TLSv1_2
                connection = http.client.HTTPSConnection(host, 443, timeout=8, context=context)
                connection.connect()
                certificate = connection.sock.getpeercert(binary_form=True)
                metadata["tls_peer_certificate_sha256"] = hashlib.sha256(certificate).hexdigest()
                headers = {"Authorization": "Bearer " + token, "Accept": "application/json",
                           "Accept-Encoding": "identity", "Connection": "close"}
                if body is not None:
                    headers["Content-Type"] = "application/json"
                connection.request(method, path, body=body, headers=headers)
                response = connection.getresponse()
                metadata.update(status=response.status, response_headers=response.getheaders())
                # http.client never follows redirects. Preserve the exact body
                # even for errors, then refuse to treat it as successful collection.
                captured = response.read(MAX_BODY + 1)
                require(len(captured) <= MAX_BODY, "response exceeds bounded body limit")
                metadata["body_complete"] = True
                require(response.getheader("Content-Encoding", "identity") == "identity",
                        "compressed responses are unsupported")
                require(response.status == 200, "HTTP response was not successful")
        except Exception as error:
            failure = error
            metadata["outcome"] = (
                "http_failure" if metadata.get("body_complete") else "unknown"
            )
            metadata["failure_kind"] = type(error).__name__
        finally:
            if connection is not None:
                connection.close()
            saved_body.write(captured)
            metadata["captured_body_sha256"] = hashlib.sha256(captured).hexdigest()
            metadata["captured_body_bytes"] = len(captured)
            metadata["finished_at_ms"] = int(time.time() * 1000)
            metadata.setdefault("outcome", "response_collected")
            saved_metadata.write((json.dumps(metadata, indent=2) + "\n").encode())
    if failure is not None:
        raise ClientError(
            "diagnostic request failed; retained response/metadata; no retry performed"
        ) from failure
    return metadata


def bridge_streams(transport, input_fd, output_fd, max_seconds, max_bytes):
    """Bounded full-duplex pump; no shell commands and no destination parsing."""
    require(1 <= max_seconds <= 300 and 1024 <= max_bytes <= 16 * 1024 * 1024,
            "invalid diagnostic bridge limits")
    end = time.monotonic() + max_seconds
    old_input, old_output = os.get_blocking(input_fd), os.get_blocking(output_fd)
    os.set_blocking(input_fd, False)
    os.set_blocking(output_fd, False)
    transport.setblocking(False)
    outgoing = bytearray()
    incoming = bytearray()
    sent = received = 0
    input_done = remote_done = False
    send_wants_read = recv_wants_write = False
    try:
        while not remote_done or incoming:
            require(time.monotonic() < end, "diagnostic bridge time limit reached")
            reads = []
            writes = []
            if not input_done and len(outgoing) < 65536:
                reads.append(input_fd)
            if not remote_done and len(incoming) < 65536:
                reads.append(transport)
            if (outgoing and not send_wants_read) or recv_wants_write:
                writes.append(transport)
            if incoming:
                writes.append(output_fd)
            ready_read, ready_write, _ = select.select(
                reads, writes, [], min(0.25, max(0, end - time.monotonic()))
            )
            if input_fd in ready_read:
                remaining = max_bytes - sent - len(outgoing)
                data = os.read(input_fd, min(65536 - len(outgoing), remaining + 1))
                require(len(data) <= remaining, "diagnostic upload limit reached")
                if data:
                    outgoing.extend(data)
                else:
                    input_done = True
            if outgoing and (
                transport in ready_write or (send_wants_read and transport in ready_read)
            ):
                try:
                    count = transport.send(outgoing)
                    require(count > 0, "TLS peer stopped accepting data")
                    del outgoing[:count]
                    sent += count
                    require(sent <= max_bytes, "diagnostic upload limit reached")
                    send_wants_read = False
                except ssl.SSLWantReadError:
                    send_wants_read = True
                except (ssl.SSLWantWriteError, BlockingIOError):
                    pass
            if not remote_done and len(incoming) < 65536 and (
                transport in ready_read or transport.pending()
                or (recv_wants_write and transport in ready_write)
            ):
                try:
                    data = transport.recv(65536 - len(incoming))
                    if data:
                        incoming.extend(data)
                        received += len(data)
                        require(received <= max_bytes, "diagnostic download limit reached")
                    else:
                        remote_done = True
                    recv_wants_write = False
                except ssl.SSLWantWriteError:
                    recv_wants_write = True
                except (ssl.SSLWantReadError, BlockingIOError):
                    pass
            if incoming and output_fd in ready_write:
                count = os.write(output_fd, incoming)
                del incoming[:count]
    finally:
        os.set_blocking(input_fd, old_input)
        os.set_blocking(output_fd, old_output)


def tls_bridge(instance_id, max_seconds=120, max_bytes=4 * 1024 * 1024):
    host = destination(instance_id, 1022)
    require(1 <= max_seconds <= 300 and 1024 <= max_bytes <= 16 * 1024 * 1024,
            "invalid diagnostic bridge limits")
    context = ssl.create_default_context()
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    # TLS identity is the gateway identity only. OpenSSH must separately enforce
    # StrictHostKeyChecking against the observed/independently verified guest key.
    with deadline(HTTP_SECONDS):
        transport = socket.create_connection((host, 443), timeout=8)
        try:
            wrapped = context.wrap_socket(transport, server_hostname=host)
        except Exception:
            transport.close()
            raise
    with wrapped:
        bridge_streams(wrapped, sys.stdin.fileno(), sys.stdout.fileno(),
                       max_seconds, max_bytes)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("identity", "quote"):
        command_parser = subparsers.add_parser(command)
        command_parser.add_argument("--instance-id", required=True)
        command_parser.add_argument("--bearer-file", type=Path, required=True)
        command_parser.add_argument("--out", type=Path, required=True)
        if command == "quote":
            command_parser.add_argument("--challenge", type=Path, required=True)
    bridge = subparsers.add_parser("tls-bridge")
    bridge.add_argument("--instance-id", required=True)
    bridge.add_argument("--max-seconds", type=int, default=120)
    bridge.add_argument("--max-bytes", type=int, default=4 * 1024 * 1024)
    args = parser.parse_args()
    try:
        if args.command == "tls-bridge":
            tls_bridge(args.instance_id, args.max_seconds, args.max_bytes)
        else:
            metadata = fetch(args.instance_id, args.bearer_file, args.out,
                             getattr(args, "challenge", None))
            print(f'Collected HTTP {metadata["status"]}; evidence remains unverified.')
            print(f'Body SHA-256: {metadata["captured_body_sha256"]}')
    except (ClientError, c.Rejected, OSError) as error:
        print(f"Diagnostic failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    main()
