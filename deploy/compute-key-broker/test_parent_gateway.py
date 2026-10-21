import http.client
import json
import threading
import unittest
from unittest.mock import patch

import parent_gateway as gateway


class FakeEnclave:
    def __init__(self):
        self.closed = False
        self.calls = []
        self.fail_release = False

    def command(self, operation, request):
        self.calls.append((operation, request))
        if operation == "release" and self.fail_release:
            raise TimeoutError("uncertain external result")
        return {"ok": True, "result": {"fake": operation}}

    def close(self):
        self.closed = True


def request():
    return {"binding": {"fake": "public"}, "recipient_spki_der_base64": "public",
            "signed_job_envelope": {"fake": "signed"}}


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.connections = []
        self.now = 10

        def connect():
            value = FakeEnclave()
            self.connections.append(value)
            return value
        self.sessions = gateway.Sessions(connect, clock=lambda: self.now)

    def tearDown(self):
        self.sessions.close()

    def test_same_connection_and_single_release(self):
        result = self.sessions.challenge(request())
        body = {"session_id": result["session_id"], "request": {"evidence": "public"}}
        self.assertEqual(self.sessions.release(body), {"fake": "release"})
        self.assertTrue(self.connections[0].closed)
        self.assertEqual([v[0] for v in self.connections[0].calls], ["challenge", "release"])
        with self.assertRaises(KeyError):
            self.sessions.release(body)

    def test_unknown_result_consumed_without_retry(self):
        result = self.sessions.challenge(request())
        self.connections[0].fail_release = True
        body = {"session_id": result["session_id"], "request": {}}
        with self.assertRaises(TimeoutError):
            self.sessions.release(body)
        with self.assertRaises(KeyError):
            self.sessions.release(body)
        self.assertEqual(len(self.connections[0].calls), 2)
        self.assertTrue(self.connections[0].closed)

    def test_expiry_capacity_and_cleanup(self):
        for _ in range(gateway.MAX_SESSIONS):
            self.sessions.challenge(request())
        with self.assertRaises(ValueError):
            self.sessions.challenge(request())
        self.now += gateway.SESSION_SECONDS
        self.sessions.prune()
        self.assertTrue(all(c.closed for c in self.connections))
        self.assertEqual(self.sessions.active, {})
        self.sessions.challenge(request())

    def test_credentials_or_policy_never_accepted_as_public_operation(self):
        for value in ({"credentials": {}}, {**request(), "policy": {}}, {**request(), "operation": "credentials"}):
            with self.assertRaises(ValueError):
                self.sessions.challenge(value)
        self.assertEqual(self.connections, [])

    def test_failed_open_does_not_leak_capacity(self):
        with patch.object(self.sessions, "connect", side_effect=OSError):
            with self.assertRaises(OSError):
                self.sessions.challenge(request())
        self.assertEqual(self.sessions.opening, 0)
        self.sessions.challenge(request())

    def test_duplicate_nonfinite_and_oversized_json(self):
        for raw in ('{"a":1,"a":2}', '{"x":NaN}'):
            with self.assertRaises(ValueError):
                gateway.strict_json(raw)
        with self.assertRaises(ValueError):
            gateway.encode({"a": "x" * gateway.MAX_FRAME})


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.sessions = gateway.Sessions(FakeEnclave)
        self.server = gateway.Gateway(("127.0.0.1", 0), self.sessions)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.sessions.close()
        self.thread.join()

    def post(self, path, raw, headers=None):
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=2)
        try:
            connection.request("POST", path, raw, headers or {"Content-Type": "application/json"})
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def test_real_http_challenge_release_and_unknown_outcome(self):
        status, result = self.post("/v1/challenge", json.dumps(request()))
        self.assertEqual(status, 200)
        self.assertEqual(set(result), {"session_id", "challenge"})
        body = json.dumps({"session_id": result["session_id"], "request": {}})
        self.assertEqual(self.post("/v1/release", body), (200, {"fake": "release"}))
        status, value = self.post("/v1/release", body)
        self.assertEqual(status, 400)
        self.assertIn("do_not_retry", value["error"])

    def test_external_bind_and_privileged_routes_rejected(self):
        with self.assertRaises(ValueError):
            gateway.Gateway(("0.0.0.0", 8088), self.sessions)
        for path in ("/credentials", "/v1/challenge?x=1", "/policy"):
            self.assertEqual(self.post(path, "{}")[0], 400)

    def test_wrong_content_type_and_duplicate_fields_rejected(self):
        self.assertEqual(self.post("/v1/challenge", "{}", {"Content-Type": "text/plain"})[0], 400)
        self.assertEqual(self.post("/v1/challenge", '{"binding":{},"binding":{}}')[0], 400)


if __name__ == "__main__":
    unittest.main()
