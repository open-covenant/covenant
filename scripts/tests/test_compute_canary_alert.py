#!/usr/bin/env python3
"""Offline alert contract tests. HTTPS is replaced; no email can be sent."""
from email.message import Message
import importlib.util
import io
import json
from pathlib import Path
import socket
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid


SCRIPT = Path(__file__).resolve().parents[1] / "compute-canary-alert.py"
SPEC = importlib.util.spec_from_file_location("compute_canary_alert", SCRIPT)
ALERT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ALERT)

ENV = {
    "COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN": "fixture-only-token",
    "COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID": "a" * 32,
    "COVENANT_CANARY_ALERT_FROM": "compute-alerts@opencovenant.org",
    "COVENANT_CANARY_ALERT_TO": "alert@opencovenant.org",
}


def payload():
    return {"schema": 1, "type": ALERT.MISSING, "state": {
        "schema": 1, "generated_at_unix": 100 * ALERT.WINDOW_SECONDS + 123,
        "cadence_seconds": ALERT.WINDOW_SECONDS, "enabled": True,
        "monitor": {"consecutive_unsuccessful_windows": 3, "alert_due": True,
                    "last_success_at_unix": None},
        "history": [{"run_id": "must-not-be-in-email", "reason": "untrusted text"}],
    }}


def accepted():
    return {"success": True, "errors": [], "result": {
        "delivered": [ALERT.RECIPIENT], "queued": [], "permanent_bounces": [],
    }}


class Response:
    def __init__(self, body=None, status=200, headers=None):
        self.body = io.BytesIO(json.dumps(accepted()).encode() if body is None else body)
        self.status = status
        self.headers = Message()
        for name, value in headers or []:
            self.headers.add_header(name, value)
        self.read_sizes = []

    def read1(self, count):
        self.read_sizes.append(count)
        return self.body.read1(count)


class Connection:
    def __init__(self, response=None):
        self.response = response or Response()
        self.requests = []
        self.closed = False

    def request(self, method, path, body, headers):
        self.requests.append((method, path, body, headers))

    def getresponse(self):
        return self.response

    def close(self):
        self.closed = True


class AlertTests(unittest.TestCase):
    def setUp(self):
        self.config = ALERT.load_config(ENV)
        self.connection = Connection()
        self.network = patch.object(ALERT.http.client, "HTTPSConnection", return_value=self.connection)
        self.factory = self.network.start()
        self.addCleanup(self.network.stop)

    def send(self, value=None):
        event_id, email = ALERT.email_event(payload() if value is None else value, self.config)
        ALERT.deliver(self.config, event_id, email)
        return event_id, email

    def invoke(self, blob=None, env=None):
        blob = json.dumps(payload()).encode() if blob is None else blob
        out, err = io.StringIO(), io.StringIO()
        with patch.object(ALERT.sys, "argv", [str(SCRIPT)]), \
                patch.object(ALERT.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(blob))), \
                patch.object(ALERT.sys, "stdout", out), patch.object(ALERT.sys, "stderr", err), \
                patch.object(ALERT.os, "environ", ENV if env is None else env):
            code = ALERT.main()
        return code, out.getvalue(), err.getvalue()

    def test_exact_cloudflare_contract_and_fixed_destination(self):
        event_id, email = self.send()
        self.assertEqual(self.factory.call_args.args, ("api.cloudflare.com",))
        self.assertEqual(self.factory.call_args.kwargs["timeout"], 5)
        self.assertEqual(self.factory.call_args.kwargs["context"].verify_mode, ALERT.ssl.CERT_REQUIRED)
        self.assertTrue(self.factory.call_args.kwargs["context"].check_hostname)
        self.assertEqual(len(self.connection.requests), 1)
        method, path, body, headers = self.connection.requests[0]
        self.assertEqual(method, "POST")
        self.assertEqual(path, "/client/v4/accounts/" + "a" * 32 + "/email/sending/send")
        self.assertEqual(headers["Authorization"], "Bearer fixture-only-token")
        self.assertEqual(headers["Idempotency-Key"], event_id)
        self.assertEqual(headers["Accept-Encoding"], "identity")
        self.assertEqual(json.loads(body), email)
        self.assertEqual(email["to"], ALERT.RECIPIENT)
        self.assertEqual(email["headers"]["X-Covenant-Event-ID"], event_id)
        self.assertNotIn("must-not-be-in-email", email["text"])
        self.assertNotIn("untrusted text", email["text"])
        self.assertTrue(self.connection.closed)

    def test_retry_event_and_message_stable_within_window(self):
        first = ALERT.email_event(payload(), self.config)
        changed = payload()
        changed["state"]["generated_at_unix"] += 60
        changed["state"]["history"] = [{"private": "do not relay"}]
        self.assertEqual(first, ALERT.email_event(changed, self.config))
        self.assertEqual(uuid.UUID(first[0]).version, 5)
        changed["state"]["generated_at_unix"] += ALERT.WINDOW_SECONDS
        self.assertNotEqual(first[0], ALERT.email_event(changed, self.config)[0])

    def test_recovery_is_distinct_and_requires_complete_success(self):
        original = ALERT.email_event(payload(), self.config)[0]
        recovered = payload()
        recovered["type"] = ALERT.RECOVERED
        recovered["state"]["monitor"] = {"consecutive_unsuccessful_windows": 0,
            "alert_due": False, "last_success_at_unix": recovered["state"]["generated_at_unix"] - 10}
        event_id, message = ALERT.email_event(recovered, self.config)
        self.assertNotEqual(original, event_id)
        self.assertIn("recovered", message["subject"])
        for invalid in (None, 0, True, recovered["state"]["generated_at_unix"] + 1):
            with self.subTest(last_success=invalid):
                recovered["state"]["monitor"]["last_success_at_unix"] = invalid
                with self.assertRaises(ALERT.Refusal):
                    ALERT.email_event(recovered, self.config)

    def test_queued_and_delivered_are_accepted(self):
        for field in ("delivered", "queued"):
            value = accepted()
            value["result"]["delivered"] = []
            value["result"][field] = [ALERT.RECIPIENT]
            self.assertTrue(ALERT.cloudflare_accepted(value))
            value["result"]["permanent_bounces"] = ["somebody-else@example.com"]
            self.assertTrue(ALERT.cloudflare_accepted(value))

    def test_permanent_bounces_and_suppression_override_acceptance(self):
        for field in ("permanent_bounces", "suppressed_recipients"):
            for disposition in ("delivered", "queued"):
                value = accepted()
                value["result"][disposition] = [ALERT.RECIPIENT]
                value["result"][field] = [ALERT.RECIPIENT]
                self.assertFalse(ALERT.cloudflare_accepted(value))

    def test_malformed_provider_contract_is_never_accepted(self):
        invalids = [None, [], {}, {"success": True}, {"success": False, "errors": [], "result": accepted()["result"]}]
        for field, values in (("success", (False, 1, "true", None)), ("errors", (None, {}, ["error"], "")),
                              ("result", (None, [], "delivered"))):
            for value in values:
                candidate = accepted()
                candidate[field] = value
                invalids.append(candidate)
        for field in ("delivered", "queued", "permanent_bounces", "suppressed_recipients"):
            for value in (None, "", {}, [1], [ALERT.RECIPIENT, False]):
                candidate = accepted()
                candidate["result"][field] = value
                invalids.append(candidate)
        for field in ("delivered", "queued", "permanent_bounces"):
            candidate = accepted()
            del candidate["result"][field]
            invalids.append(candidate)
        for candidate in invalids:
            with self.subTest(value=candidate):
                self.assertFalse(ALERT.cloudflare_accepted(candidate))

    def test_other_recipient_or_similar_case_does_not_acknowledge(self):
        for recipient in ("other@example.com", "Alert@opencovenant.org", ALERT.RECIPIENT + " "):
            value = accepted()
            value["result"]["delivered"] = [recipient]
            self.assertFalse(ALERT.cloudflare_accepted(value))

    def test_200_false_is_nonzero_without_provider_detail(self):
        self.connection.response = Response(body=json.dumps({"success": False, "errors": ["secret-provider-body"]}).encode())
        self.assertEqual(self.invoke(), (1, "", "canary alert not accepted\n"))

    def test_success_is_silent_exit_zero(self):
        self.assertEqual(self.invoke(), (0, "", ""))

    def test_no_redirect_or_http_error_is_retried(self):
        for status in (301, 302, 303, 307, 308, 400, 401, 429, 500, 503):
            with self.subTest(status=status):
                self.connection = Connection(Response(status=status, headers=[("Location", "https://attacker.example/token")]))
                self.factory.return_value = self.connection
                with self.assertRaises(ALERT.Refusal):
                    self.send()
                self.assertEqual(len(self.connection.requests), 1)
                self.assertEqual(self.connection.response.read_sizes, [])
                self.assertTrue(self.connection.closed)

    def test_no_environment_proxy_can_change_fixed_origin(self):
        with patch.dict(ALERT.os.environ, {"HTTPS_PROXY": "https://attacker.example", "ALL_PROXY": "https://attacker.example"}):
            self.send()
        self.assertEqual(self.factory.call_args.args, (ALERT.HOST,))
        self.assertEqual(len(self.connection.requests), 1)

    def test_transport_failure_never_retries(self):
        for failure in (socket.timeout("secret timeout message"), OSError("secret failure"), ALERT.http.client.RemoteDisconnected("secret")):
            with self.subTest(kind=type(failure).__name__):
                self.factory.reset_mock()
                self.factory.side_effect = failure
                self.assertEqual(self.invoke(), (1, "", "canary alert not accepted\n"))
                self.assertEqual(self.factory.call_count, 1)

    def test_whole_request_deadline_interrupts_dribbling_body(self):
        with patch.object(self.connection.response, "read1", side_effect=lambda _size: time.sleep(1)), \
                patch.object(ALERT, "TIMEOUT_SECONDS", 0.02):
            before = time.monotonic()
            with self.assertRaises(ALERT.Refusal):
                self.send()
            self.assertLess(time.monotonic() - before, 0.5)
        self.assertTrue(self.connection.closed)

    def test_response_length_limit_rejects_before_read(self):
        self.connection.response = Response(headers=[("Content-Length", str(ALERT.MAX_RESPONSE_BYTES + 1))])
        with self.assertRaises(ALERT.Refusal):
            self.send()
        self.assertEqual(self.connection.response.read_sizes, [])

    def test_streamed_response_limit_and_exact_boundary(self):
        good = json.dumps(accepted()).encode()
        exact = good + b" " * (ALERT.MAX_RESPONSE_BYTES - len(good))
        self.connection.response = Response(body=exact)
        self.send()
        self.connection.response = Response(body=exact + b" ")
        with self.assertRaises(ALERT.Refusal):
            self.send()
        self.assertLessEqual(self.connection.response.body.tell(), ALERT.MAX_RESPONSE_BYTES + 1)

    def test_provider_response_truncation_is_rejected(self):
        good = json.dumps(accepted()).encode()
        for body, headers in ((good[:-1], []), (good, [("Content-Length", str(len(good) + 1))])):
            self.connection.response = Response(body=body, headers=headers)
            with self.assertRaises(ALERT.Refusal):
                self.send()

    def test_ambiguous_or_compressed_response_is_rejected(self):
        cases = [[("Content-Length", "3"), ("Content-Length", "4")],
                 [("Content-Length", "-1")], [("Content-Length", "wat")],
                 [("Content-Encoding", "gzip")], [("Content-Encoding", "identity"), ("Content-Encoding", "identity")],
                 [("Content-Length", "3"), ("Transfer-Encoding", "chunked")]]
        for headers in cases:
            with self.subTest(headers=headers):
                self.connection.response = Response(headers=headers)
                with self.assertRaises(ALERT.Refusal):
                    self.send()
                self.assertEqual(self.connection.response.read_sizes, [])

    def test_strict_json_on_stdin_and_provider_response(self):
        candidates = [b"{", b"\xff", b'{"success":true,"success":true}', b'{"number":NaN}',
                      b'{"number":Infinity}', b'[]\n{}']
        for blob in candidates:
            with self.subTest(blob=blob):
                with self.assertRaises(ALERT.Refusal):
                    ALERT.parse_json(blob)
                self.connection.response = Response(body=blob)
                self.assertEqual(self.invoke(), (1, "", "canary alert not accepted\n"))
                self.factory.reset_mock()
                self.assertEqual(self.invoke(blob), (1, "", "canary alert not accepted\n"))
                self.factory.assert_not_called()

    def test_invalid_input_never_sends(self):
        invalids = [None, [], {}, {"schema": True}, {"schema": 1, "type": "send_anything"}]
        changes = [("schema", True), ("cadence_seconds", 1), ("generated_at_unix", True),
                   ("generated_at_unix", -1), ("generated_at_unix", 1.5), ("enabled", False)]
        for key, value in changes:
            candidate = payload()
            candidate["state"][key] = value
            invalids.append(candidate)
        for key, value in (("consecutive_unsuccessful_windows", 2), ("consecutive_unsuccessful_windows", True), ("alert_due", False)):
            candidate = payload()
            candidate["state"]["monitor"][key] = value
            invalids.append(candidate)
        for candidate in invalids:
            with self.subTest(value=candidate):
                self.assertEqual(self.invoke(json.dumps(candidate).encode()), (1, "", "canary alert not accepted\n"))
        self.factory.assert_not_called()

    def test_oversize_input_never_sends(self):
        self.assertEqual(self.invoke(b" " * (ALERT.MAX_INPUT_BYTES + 1)), (1, "", "canary alert not accepted\n"))
        self.factory.assert_not_called()

    def test_configuration_cannot_redirect_secrets_or_recipient(self):
        for field in ENV:
            candidate = dict(ENV)
            del candidate[field]
            with self.assertRaises(ALERT.Refusal):
                ALERT.load_config(candidate)
        bad = {
            "COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN": ["", "x\r\nAuthorization: x", "x\x00", " space ", "x" * 4097],
            "COVENANT_CANARY_ALERT_CLOUDFLARE_ACCOUNT_ID": ["a" * 31, "g" * 32, "../../attacker", "a" * 32 + "/"],
            "COVENANT_CANARY_ALERT_FROM": ["Name <mail@example.com>", "a@example.com\nBcc:x@y.com", "a@example.com,b@example.com", "a b@example.com"],
            "COVENANT_CANARY_ALERT_TO": ["attacker@example.com", ALERT.RECIPIENT + "\r\n", ALERT.RECIPIENT.upper()],
        }
        for field, values in bad.items():
            for value in values:
                with self.subTest(field=field, value=value):
                    env = {**ENV, field: value}
                    self.assertEqual(self.invoke(env=env), (1, "", "canary alert not accepted\n"))
        self.factory.assert_not_called()


if __name__ == "__main__":
    unittest.main()
