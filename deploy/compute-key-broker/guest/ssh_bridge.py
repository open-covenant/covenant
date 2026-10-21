#!/usr/bin/env python3
"""Public-only buyer SSH transport to the isolated application-key recipient.

Run with the existing verified SSH channel, as the lease buyer. No arguments,
private keys, quote API or writable policy enter this bridge. A native client
may embed this fixed source as `python3 -c SOURCE` on that channel.
"""
import os
import selectors
import socket
import sys
import time

SOCKET_PATH = "/key-broker-io/recipient.sock"
MAX_BYTES = 2 * 1024 * 1024
MAX_SECONDS = 120


def write_bounded(target, data, end):
    was_blocking = os.get_blocking(target)
    os.set_blocking(target, False)
    try:
        with selectors.DefaultSelector() as writable:
            writable.register(target, selectors.EVENT_WRITE)
            offset = 0
            while offset < len(data):
                remaining = end - time.monotonic()
                if remaining <= 0 or not writable.select(remaining):
                    return False
                try:
                    offset += os.write(target, data[offset:])
                except BlockingIOError:
                    continue
        return True
    finally:
        os.set_blocking(target, was_blocking)


def bridge(source, target, connection, *, seconds=MAX_SECONDS, maximum=MAX_BYTES):
    """Real byte forwarding; constructor-only timing seams support offline tests."""
    end = time.monotonic() + seconds
    total = 0
    with selectors.DefaultSelector() as selector:
        selector.register(source, selectors.EVENT_READ, "input")
        selector.register(connection, selectors.EVENT_READ, "output")
        while time.monotonic() < end and selector.get_map():
            remaining = end - time.monotonic()
            if remaining <= 0:
                return
            for event, _ in selector.select(min(1, remaining)):
                remaining = end - time.monotonic()
                if remaining <= 0:
                    return
                connection.settimeout(remaining)
                if event.data == "input":
                    data = os.read(source, 16384)
                    if not data:
                        selector.unregister(source)
                        connection.shutdown(socket.SHUT_WR)
                        continue
                else:
                    data = connection.recv(16384)
                    if not data:
                        return
                total += len(data)
                if total > maximum:
                    raise ValueError("public protocol exceeded byte limit")
                if event.data == "input":
                    connection.sendall(data)
                else:
                    # The verified native client continuously consumes stdout.
                    # Poll write readiness to keep a stalled client bounded.
                    if not write_bounded(target, data, end):
                        return


def main():
    if sys.argv[1:] or os.geteuid() != 10001 or os.getegid() != 10001:
        raise ValueError("buyer SSH identity required")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(5)
        connection.connect(SOCKET_PATH)
        bridge(sys.stdin.fileno(), sys.stdout.fileno(), connection)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        print('{"event":"key_recipient_transport_failed"}', file=sys.stderr)
        raise SystemExit(1)
