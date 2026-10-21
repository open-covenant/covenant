"""Real offline Unix/pipe transport; never a hardware acceptance test."""
import os
import socket
import threading
import time
import unittest
from unittest import mock

import ssh_bridge as bridge
import recipient


class PublicTransportTests(unittest.TestCase):
    def test_interactive_public_messages_and_input_half_close(self):
        inbound, client_input = os.pipe()
        client_output, outbound = os.pipe()
        local, recipient_socket = socket.socketpair()
        for sock in (local, recipient_socket):
            sock.settimeout(1)
        errors = []
        def run():
            try:
                bridge.bridge(inbound, outbound, local, seconds=1)
            except Exception as error:
                errors.append(error)
        thread = threading.Thread(target=run)
        thread.start()
        try:
            recipient_socket.sendall(b'{"hello":"public"}\n')
            self.assertEqual(os.read(client_output, 4096), b'{"hello":"public"}\n')
            os.write(client_input, b'{"challenge":"public"}\n')
            self.assertEqual(recipient_socket.recv(4096), b'{"challenge":"public"}\n')
            os.close(client_input)
            client_input = None
            self.assertEqual(recipient_socket.recv(1), b"")
            recipient_socket.sendall(b'{"receipt":"delivered"}\n')
            self.assertEqual(os.read(client_output, 4096), b'{"receipt":"delivered"}\n')
            recipient_socket.shutdown(socket.SHUT_WR)
            thread.join(timeout=1)
            self.assertFalse(thread.is_alive())
            self.assertEqual(errors, [])
        finally:
            local.close()
            recipient_socket.close()
            for fd in (inbound, client_input, client_output, outbound):
                if fd is not None:
                    os.close(fd)
            thread.join(timeout=1)

    def test_stdout_backpressure_cannot_block_past_deadline(self):
        reader, writer = os.pipe()
        try:
            started = time.monotonic()
            self.assertFalse(bridge.write_bounded(writer, b"x" * (1024 * 1024), started + 0.05))
            self.assertLess(time.monotonic() - started, 0.5)
            self.assertTrue(os.get_blocking(writer))
        finally:
            os.close(reader)
            os.close(writer)

    def test_bridge_refuses_arguments_and_wrong_identity_before_connect(self):
        for args, uid, gid in [(["bridge", "other-path"], 10001, 10001),
                               (["bridge"], 0, 0), (["bridge"], 10001, 0)]:
            with mock.patch.object(bridge.sys, "argv", args), \
                 mock.patch.object(bridge.os, "geteuid", return_value=uid), \
                 mock.patch.object(bridge.os, "getegid", return_value=gid), \
                 mock.patch.object(bridge.socket, "socket") as connect:
                with self.assertRaises(ValueError):
                    bridge.main()
                connect.assert_not_called()

    def test_service_guard_requires_whole_noswap_bounded_ipc_mount(self):
        prefix = '44 2 0:3 / /key-broker-io rw,nosuid,nodev,noexec - tmpfs tmpfs '
        guard = recipient.ServiceRuntimeGuard()
        with mock.patch.object(recipient.RuntimeGuard, "check"):
            for options in ['rw,noswap,size=1024k', 'rw,size=1024k', 'rw,noswap,size=2048k']:
                with self.subTest(options=options), \
                     mock.patch.object(recipient, "bounded_read", return_value=prefix+options+'\n'):
                    if options == 'rw,noswap,size=1024k':
                        guard.check('ssh')
                    else:
                        with self.assertRaises(ValueError):
                            guard.check('ssh')


if __name__ == '__main__':
    unittest.main()
