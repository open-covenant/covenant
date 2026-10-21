import copy
import io
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest import mock

import client
import collector as c
import render
from test_collector import INFO, NOW, POLICY, PUBLIC

INSTANCE = "fc551bcb5f96283763f42da3578805ec7e2ecfe3"


class Response:
    def __init__(self, body=b'{"diagnostic":"unverified"}\n', status=200):
        self.body, self.status = body, status
    def read(self, maximum):
        return self.body[:maximum]
    def getheaders(self):
        return [("Content-Type", "application/json")]
    def getheader(self, _name, default):
        return default


class Connection:
    def __init__(self, response=None):
        self.response = response or Response()
        self.requests = []
        self.sock = self
    def connect(self):
        pass
    def getpeercert(self, binary_form=False):
        assert binary_form
        return b"fake test certificate"
    def request(self, *arguments, **keywords):
        self.requests.append((arguments, keywords))
    def getresponse(self):
        return self.response
    def close(self):
        pass


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.token = self.directory / "bearer"
        self.token.write_text("ab" * 32 + "\n")
        self.token.chmod(0o600)
        self.out = self.directory / "response.json"
        self.challenge = {
            "schema_version": 1, "challenge_id": "b61bf342-b2ea-4012-8946-6bd69c6f333d",
            "nonce_hex": "cd" * 32, "issued_at_ms": NOW, "expires_at_ms": NOW + 120000,
            "scope": {key: POLICY[key] for key in (
                "job_id", "operator_pubkey_b58", "buyer_pubkey_b58",
                "lease_generation", "lease_expires_at_ms",
            )},
        }
        self.challenge["scope"].update(
            host=client.destination(INSTANCE, 1022), port=443, username="covenant",
            guest_channel_key=c.ssh_key(PUBLIC), workload_sha256=INFO["compose_hash"],
            transport={"kind": "tls"},
        )

    def save_challenge(self, challenge=None):
        path = self.directory / "challenge.json"
        path.write_text(json.dumps(challenge or self.challenge))
        path.chmod(0o600)
        return path

    def test_destination_is_fixed_instance_and_gateway(self):
        self.assertEqual(client.destination(INSTANCE, 18090),
                         f"{INSTANCE}-18090.dstack-pha-prod5.phala.network")
        for bad in ("https://other.example", INSTANCE.upper(), INSTANCE + ".evil", "ab"):
            with self.subTest(value=bad), self.assertRaises(client.ClientError):
                client.destination(bad, 1022)
        for port in (80, 8090):
            with self.subTest(port=port), self.assertRaises(client.ClientError):
                client.destination(INSTANCE, port)

    def test_rendered_collector_mapping_avoids_reserved_guest_agent_port(self):
        # Pinned dstack 7f143148 guest-agent/dstack.toml binds host 8090 before
        # Docker starts. The published port must match the diagnostic client
        # while retaining the image's internal 8090 listener.
        template = Path(__file__).with_name("compose.template.yaml").read_text()
        rendered = render.render(template, "sha256:" + "ab" * 32, POLICY, NOW)
        collector = rendered.split("  collector:\n", 1)[1].split("\nvolumes:\n", 1)[0]
        mappings = [
            json.loads(line.partition(":")[2].strip())
            for line in collector.splitlines() if line.strip().startswith("ports:")
        ]
        self.assertEqual(mappings, [["18090:8090"]])
        published, internal = map(int, mappings[0][0].split(":"))
        self.assertNotEqual(published, 8090)
        self.assertEqual(internal, 8090)
        self.assertEqual(client.destination(INSTANCE, published),
                         f"{INSTANCE}-18090.dstack-pha-prod5.phala.network")

    def test_token_must_be_private_regular_owned_file_without_symlink(self):
        self.assertEqual(client.read_token(self.token), "ab" * 32)
        self.token.chmod(0o644)
        with self.assertRaises(client.ClientError):
            client.read_token(self.token)
        self.token.chmod(0o600)
        link = self.directory / "link"
        link.symlink_to(self.token)
        with self.assertRaises(OSError):
            client.read_token(link)
        self.token.write_text("not a collector token")
        with self.assertRaises(client.ClientError):
            client.read_token(self.token)

    def test_challenge_endpoint_expiry_and_original_body_preserved(self):
        path = self.save_challenge()
        self.assertEqual(client.checked_challenge(path, INSTANCE, NOW), path.read_bytes())
        wrong = copy.deepcopy(self.challenge)
        wrong["scope"]["host"] = "other.example"
        path = self.save_challenge(wrong)
        with self.assertRaises(client.ClientError):
            client.checked_challenge(path, INSTANCE, NOW)
        path = self.save_challenge()
        with self.assertRaises(client.ClientError):
            client.checked_challenge(path, INSTANCE, NOW + 120000)

    def test_https_raw_response_and_private_metadata_saved(self):
        raw = b'{ "x": 1, "verification_status" : "unverified" }\n'
        connection = Connection(Response(raw))
        with mock.patch.object(client.http.client, "HTTPSConnection", return_value=connection) as factory:
            result = client.fetch(INSTANCE, self.token, self.out)
        self.assertEqual(factory.call_count, 1)
        self.assertEqual(factory.call_args.args[:2], (client.destination(INSTANCE, 18090), 443))
        self.assertTrue(factory.call_args.kwargs["context"].check_hostname)
        self.assertEqual(self.out.read_bytes(), raw)
        self.assertEqual(self.out.stat().st_mode & 0o777, 0o600)
        self.assertEqual(result["verification_status"], "unverified")
        metadata = self.out.with_name(self.out.name + ".metadata.json")
        self.assertEqual(metadata.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("ab" * 32, metadata.read_text())
        self.assertEqual(result["outcome"], "response_collected")

    def test_redirect_saved_without_follow_or_retry(self):
        connection = Connection(Response(b"redirect", 302))
        with mock.patch.object(client.http.client, "HTTPSConnection", return_value=connection) as factory:
            with self.assertRaises(client.ClientError):
                client.fetch(INSTANCE, self.token, self.out)
        self.assertEqual(factory.call_count, 1)
        self.assertEqual(len(connection.requests), 1)
        self.assertEqual(self.out.read_bytes(), b"redirect")
        metadata = json.loads(self.out.with_name(self.out.name + ".metadata.json").read_text())
        self.assertEqual(metadata["status"], 302)
        self.assertEqual(metadata["automatic_retries"], 0)

    def test_existing_output_prevents_any_network_request(self):
        self.out.write_text("retained")
        with mock.patch.object(client.http.client, "HTTPSConnection") as factory:
            with self.assertRaises(FileExistsError):
                client.fetch(INSTANCE, self.token, self.out)
        factory.assert_not_called()
        self.assertEqual(self.out.read_text(), "retained")

    def test_http_post_keeps_exact_challenge_bytes(self):
        path = self.save_challenge()
        connection = Connection()
        with mock.patch.object(client.time, "time", return_value=NOW / 1000):
            with mock.patch.object(client.http.client, "HTTPSConnection", return_value=connection):
                client.fetch(INSTANCE, self.token, self.out, path)
        self.assertEqual(connection.requests[0][0], ("POST", "/quote"))
        self.assertEqual(connection.requests[0][1]["body"], path.read_bytes())

    def test_oversized_response_marked_incomplete_and_never_retried(self):
        connection = Connection(Response(b"x" * (client.MAX_BODY + 2)))
        with mock.patch.object(client.http.client, "HTTPSConnection", return_value=connection):
            with self.assertRaises(client.ClientError):
                client.fetch(INSTANCE, self.token, self.out)
        metadata = json.loads(self.out.with_name(self.out.name + ".metadata.json").read_text())
        self.assertFalse(metadata["body_complete"])
        self.assertEqual(metadata["outcome"], "unknown")
        self.assertEqual(len(connection.requests), 1)

    def test_transport_failure_retains_unknown_outcome(self):
        connection = Connection()
        connection.getresponse = mock.Mock(side_effect=TimeoutError())
        with mock.patch.object(client.http.client, "HTTPSConnection", return_value=connection):
            with self.assertRaises(client.ClientError):
                client.fetch(INSTANCE, self.token, self.out)
        metadata = json.loads(self.out.with_name(self.out.name + ".metadata.json").read_text())
        self.assertEqual(metadata["outcome"], "unknown")
        self.assertEqual(metadata["failure_kind"], "TimeoutError")

    def test_bridge_full_duplex_without_shell(self):
        left, right = socket.socketpair()
        input_read, input_write = os.pipe()
        output_read, output_write = os.pipe()
        class Transport:
            def fileno(self):
                return left.fileno()
            def setblocking(self, flag):
                left.setblocking(flag)
            def send(self, data):
                return left.send(data)
            def recv(self, size):
                return left.recv(size)
            def pending(self):
                return 0
        def remote():
            with right:
                self.assertEqual(right.recv(1024), b"ssh synthetic input")
                right.sendall(b"ssh synthetic output")
        thread = threading.Thread(target=remote, daemon=True)
        thread.start()
        try:
            os.write(input_write, b"ssh synthetic input")
            os.close(input_write)
            input_write = None
            client.bridge_streams(Transport(), input_read, output_write, 2, 1024)
            self.assertEqual(os.read(output_read, 1024), b"ssh synthetic output")
            self.assertTrue(os.get_blocking(input_read))
            self.assertTrue(os.get_blocking(output_write))
        finally:
            left.close()
            right.close()
            for descriptor in (input_read, input_write, output_read, output_write):
                if descriptor is not None:
                    os.close(descriptor)
            thread.join(timeout=2)

    def test_bridge_limit_validation(self):
        for seconds, size in ((0, 1024), (301, 1024), (2, 17 * 1024 * 1024)):
            with self.assertRaises(client.ClientError):
                client.bridge_streams(None, 0, 1, seconds, size)

    def test_bridge_rejects_upload_overflow_before_forwarding(self):
        left, right = socket.socketpair()
        input_read, input_write = os.pipe()
        output_read, output_write = os.pipe()
        class Transport:
            def fileno(self):
                return left.fileno()
            def setblocking(self, flag):
                left.setblocking(flag)
            def send(self, data):
                return left.send(data)
            def recv(self, size):
                return left.recv(size)
            def pending(self):
                return 0
        try:
            os.write(input_write, b"x" * 1025)
            with self.assertRaises(client.ClientError):
                client.bridge_streams(Transport(), input_read, output_write, 2, 1024)
            right.setblocking(False)
            with self.assertRaises(BlockingIOError):
                right.recv(2048)
        finally:
            left.close()
            right.close()
            for descriptor in (input_read, input_write, output_read, output_write):
                os.close(descriptor)

    def test_tls_hostname_validation_and_fixed_destination(self):
        plain = mock.MagicMock()
        wrapped = mock.MagicMock()
        wrapped.__enter__.return_value = wrapped
        context = mock.Mock()
        context.wrap_socket.return_value = wrapped
        with mock.patch.object(client.ssl, "create_default_context", return_value=context):
            with mock.patch.object(client.socket, "create_connection", return_value=plain) as connect:
                with mock.patch.object(client, "bridge_streams") as pump:
                    with mock.patch.object(client.sys, "stdin", io.StringIO()):
                        # StringIO has no fd; use the real input descriptor explicitly.
                        with mock.patch.object(client.sys.stdin, "fileno", return_value=0):
                            client.tls_bridge(INSTANCE)
        connect.assert_called_once_with((client.destination(INSTANCE, 1022), 443), timeout=8)
        context.wrap_socket.assert_called_once_with(
            plain, server_hostname=client.destination(INSTANCE, 1022))
        pump.assert_called_once()


if __name__ == "__main__":
    unittest.main()
