"""Offline real socket transport tests; no Nitro, NSM, or AWS claims."""
import socket
import subprocess
import threading
import time
import unittest
from unittest import mock

import enclave_entrypoint as entry


class BridgeTests(unittest.TestCase):
    def bridge(self, seconds=1.0, maximum=4096):
        client, left = socket.socketpair()
        right, upstream = socket.socketpair()
        errors = []
        for sock in (client, left, right, upstream):
            sock.settimeout(1)

        def run():
            try:
                with left, right:
                    entry.bridge(left, right, seconds, maximum)
            except Exception as error:
                errors.append(error)

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        self.addCleanup(client.close)
        self.addCleanup(upstream.close)
        self.addCleanup(lambda: thread.join(timeout=2))
        return client, upstream, thread, errors

    def test_interactive_forwarding_both_directions(self):
        client, upstream, thread, errors = self.bridge()
        client.sendall(b'{"challenge":"public"}\n')
        self.assertEqual(upstream.recv(4096), b'{"challenge":"public"}\n')
        upstream.sendall(b'{"response":"ciphertext"}\n')
        self.assertEqual(client.recv(4096), b'{"response":"ciphertext"}\n')
        client.shutdown(socket.SHUT_WR)
        upstream.shutdown(socket.SHUT_WR)
        thread.join(timeout=1)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])

    def test_input_half_close_preserves_last_response(self):
        client, upstream, thread, errors = self.bridge()
        client.sendall(b"enrollment request\n")
        client.shutdown(socket.SHUT_WR)
        self.assertEqual(upstream.recv(4096), b"enrollment request\n")
        self.assertEqual(upstream.recv(1), b"")
        upstream.sendall(b"attested enrollment response\n")
        upstream.shutdown(socket.SHUT_WR)
        self.assertEqual(client.recv(4096), b"attested enrollment response\n")
        self.assertEqual(client.recv(1), b"")
        thread.join(timeout=1)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])

    def test_byte_budget_counts_both_directions_and_never_forwards_excess(self):
        client, upstream, thread, errors = self.bridge(maximum=12)
        client.sendall(b"123456")
        self.assertEqual(upstream.recv(6), b"123456")
        upstream.sendall(b"abcdef")
        self.assertEqual(client.recv(6), b"abcdef")
        client.sendall(b"x")
        thread.join(timeout=1)
        self.assertFalse(thread.is_alive())
        self.assertEqual(upstream.recv(1), b"")
        self.assertEqual(errors, [])

    def test_idle_deadline_does_not_use_fixed_socket_timeout(self):
        started = time.monotonic()
        client, _, thread, errors = self.bridge(seconds=0.08)
        thread.join(timeout=0.75)
        self.assertFalse(thread.is_alive())
        self.assertLess(time.monotonic() - started, 0.75)
        self.assertEqual(client.recv(1), b"")
        self.assertEqual(errors, [])

    def test_blocked_write_is_limited_by_overall_deadline(self):
        client, upstream, thread, errors = self.bridge(seconds=0.12, maximum=2 * 1024 * 1024)
        upstream.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
        client.settimeout(0.01)
        started = time.monotonic()
        # No downstream reader: a blocked sendall must not inherit 20/75 sec.
        try:
            for _ in range(256):
                client.sendall(b"x" * 16384)
        except (OSError, TimeoutError):
            pass
        thread.join(timeout=0.75)
        self.assertFalse(thread.is_alive())
        self.assertLess(time.monotonic() - started, 0.75)
        self.assertTrue(all(isinstance(error, (OSError, TimeoutError)) for error in errors))


class EntrypointTests(unittest.TestCase):
    def test_missing_hardware_stops_before_network_or_child(self):
        with mock.patch.object(entry.resource, "setrlimit") as limits, \
             mock.patch.object(entry.os, "umask"), \
             mock.patch.object(entry.Path, "exists", return_value=False), \
             mock.patch.object(entry.subprocess, "run") as command, \
             mock.patch.object(entry.socket, "socket") as create:
            with self.assertRaisesRegex(RuntimeError, "Nitro hardware required"):
                entry.main()
            limits.assert_called_once_with(entry.resource.RLIMIT_CORE, (0, 0))
            command.assert_not_called()
            create.assert_not_called()

    def test_protocol_failure_kills_child_closes_fds_and_releases_capacity(self):
        peer, incoming = socket.socketpair()
        child_process, capacity, endpoints = mock.Mock(), mock.Mock(), []
        original = socket.socketpair

        def pair():
            result = original()
            endpoints.extend(result)
            return result

        try:
            with mock.patch.object(entry, "MAX_CONNECTIONS", capacity), \
                 mock.patch.object(entry.socket, "socketpair", side_effect=pair), \
                 mock.patch.object(entry.subprocess, "Popen", return_value=child_process) as spawn, \
                 mock.patch.object(entry, "bridge", side_effect=OSError("synthetic failure")):
                entry.protocol_connection(incoming, "serve")
            child_process.kill.assert_called_once()
            child_process.wait.assert_called_once_with(timeout=5)
            capacity.release.assert_called_once()
            self.assertTrue(all(sock.fileno() == -1 for sock in endpoints))
            self.assertEqual(incoming.fileno(), -1)
            self.assertEqual(spawn.call_args.args[0],
                             ["/usr/local/bin/covenant-compute-key-broker", "serve"])
            self.assertEqual(spawn.call_args.kwargs["stderr"], subprocess.DEVNULL)
            self.assertEqual(spawn.call_args.kwargs["env"], {"PATH": "/usr/local/bin:/usr/bin:/bin"})
            self.assertTrue(spawn.call_args.kwargs["close_fds"])
        finally:
            peer.close()
            incoming.close()

    def test_spawn_failure_closes_fds_and_releases_capacity(self):
        peer, incoming = socket.socketpair()
        capacity, endpoints = mock.Mock(), []
        original = socket.socketpair

        def pair():
            result = original()
            endpoints.extend(result)
            return result

        try:
            with mock.patch.object(entry, "MAX_CONNECTIONS", capacity), \
                 mock.patch.object(entry.socket, "socketpair", side_effect=pair), \
                 mock.patch.object(entry.subprocess, "Popen", side_effect=OSError("synthetic failure")):
                entry.protocol_connection(incoming, "enroll-signing")
            capacity.release.assert_called_once()
            self.assertTrue(all(sock.fileno() == -1 for sock in endpoints))
            self.assertEqual(incoming.fileno(), -1)
        finally:
            peer.close()
            incoming.close()


if __name__ == "__main__":
    unittest.main()
