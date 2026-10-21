#!/usr/bin/env python3
"""Nitro transport only. AWS TLS remains end-to-end inside the Rust process.

The parent runs the AWS vsock-proxy on CID 3:8000. It forwards bytes to the
regional KMS host:443 and cannot terminate TLS. The protocol listener is 5000.
No plaintext application keys or private signing/recipient keys cross either
socket. Do not run this image with Nitro debug mode or console capture.
"""
import json
import os
import re
import resource
import selectors
import socket
import subprocess
import threading
import time
from pathlib import Path

MAX_CONNECTIONS = threading.BoundedSemaphore(2)


def bridge(left, right, seconds, maximum):
    """Bound both directions and total session time; no payload logging."""
    selector = selectors.DefaultSelector()
    end = time.monotonic() + seconds
    transferred = 0
    try:
        selector.register(left, selectors.EVENT_READ, right)
        selector.register(right, selectors.EVENT_READ, left)
        while selector.get_map():
            remaining = end - time.monotonic()
            if remaining <= 0:
                return
            for selected, _ in selector.select(min(1, remaining)):
                remaining = end - time.monotonic()
                if remaining <= 0:
                    return
                selected.fileobj.settimeout(remaining)
                data = selected.fileobj.recv(16384)
                if not data:
                    selector.unregister(selected.fileobj)
                    selected.data.shutdown(socket.SHUT_WR)
                    continue
                transferred += len(data)
                if transferred > maximum:
                    return
                remaining = end - time.monotonic()
                if remaining <= 0:
                    return
                selected.data.settimeout(remaining)
                selected.data.sendall(data)
    finally:
        selector.close()


def kms_connection(connection):
    with connection, socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM) as parent:
        connection.settimeout(20)
        parent.settimeout(20)
        parent.connect((3, 8000))
        bridge(connection, parent, 20, 2 * 1024 * 1024)


def protocol_connection(connection, mode):
    try:
        with connection:
            connection.settimeout(75)
            # A socketpair preserves interactive challenge -> release exchange.
            # Both ends stay inside the enclave; the Rust output is ciphertext.
            local, child = socket.socketpair()
            try:
                process = subprocess.Popen(
                    ["/usr/local/bin/covenant-compute-key-broker", mode],
                    stdin=child, stdout=child, stderr=subprocess.DEVNULL,
                    env={"PATH": "/usr/local/bin:/usr/bin:/bin"}, close_fds=True,
                )
                child.close()
                try:
                    local.settimeout(75)
                    bridge(connection, local, 75, 2 * 1024 * 1024)
                finally:
                    process.kill()
                    process.wait(timeout=5)
            finally:
                local.close()
                child.close()
    except (OSError, subprocess.SubprocessError):
        pass
    finally:
        MAX_CONNECTIONS.release()


def kms_listener():
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 443))
        listener.listen(4)
        while True:
            connection, _ = listener.accept()
            # Only bounded local broker children can access enclave loopback.
            threading.Thread(target=kms_connection, args=(connection,), daemon=True).start()


def main():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    os.umask(0o077)
    if not Path("/dev/nsm").exists() or not hasattr(socket, "AF_VSOCK"):
        raise RuntimeError("Nitro hardware required")
    policy = json.loads(Path("/opt/covenant/broker-policy.json").read_bytes())
    arn = policy["signing"]["kms_key_arn"] if "broker" in policy else policy["kms_key_arn"]
    region = arn.split(":")[3]
    if not re.fullmatch(r"[a-z]{2}(?:-[a-z]+)+-[0-9]", region):
        raise RuntimeError("invalid embedded region")
    subprocess.run(["/usr/sbin/ip", "link", "set", "lo", "up"], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with open("/etc/hosts", "a", encoding="ascii") as hosts:
        hosts.write(f"\n127.0.0.1 kms.{region}.amazonaws.com\n")
    threading.Thread(target=kms_listener, daemon=True).start()
    mode = "serve" if "broker" in policy else "enroll-signing"
    with socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM) as listener:
        listener.bind((socket.VMADDR_CID_ANY, 5000))
        listener.listen(2)
        while True:
            connection, _ = listener.accept()
            if not MAX_CONNECTIONS.acquire(blocking=False):
                connection.close()
                continue
            threading.Thread(target=protocol_connection, args=(connection, mode), daemon=True).start()


if __name__ == "__main__":
    main()
