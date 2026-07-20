import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from covenant_blockrun import (  # noqa: E402
    ReceiptTransport,
    build_receipt,
    canonical_sha256_hex,
    canonicalize,
    decode_challenge,
    payment_from_accept,
    pick_accept,
    verify_receipt,
)

# The exact base64 BlockRun returns for a default gpt-4o-mini 402.
LIVE_B64 = (
    "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0cyI6W3sic2NoZW1lIjoiZXhhY3QiLCJuZXR3b3JrIjoiZWlwMTU1Ojg0NTMi"
    "LCJhbW91bnQiOiIzMDAwIiwiYXNzZXQiOiIweDgzMzU4OWZDRDZlRGI2RTA4ZjRjN0MzMkQ0ZjcxYjU0YmRBMDI5MTMi"
    "LCJwYXlUbyI6IjB4ZTkwMzAwMTRGNURBZTIxN2QwQTE1MmYwMkEwNDM1NjdiMTZjMWFCZiIsIm1heFRpbWVvdXRTZWNv"
    "bmRzIjozMDAsImV4dHJhIjp7Im5hbWUiOiJVU0QgQ29pbiIsInZlcnNpb24iOiIyIn19XSwicmVzb3VyY2UiOnsidXJs"
    "IjoiaHR0cHM6Ly9ibG9ja3J1bi5haS9hcGkvdjEvY2hhdC9jb21wbGV0aW9ucyIsImRlc2NyaXB0aW9uIjoiR1BULTRv"
    "IE1pbmkgQVBJIGNhbGwgKH4xNyBpbnB1dCwgODE5MiBtYXggb3V0cHV0IHRva2VucykiLCJtaW1lVHlwZSI6ImFwcGxp"
    "Y2F0aW9uL2pzb24ifX0="
)


def _payment():
    return payment_from_accept(
        {"network": "eip155:8453", "asset": "0x8335", "amount": "3000", "payTo": "0xe903"},
        tx="0xabc",
    )


def test_jcs_sorts_keys():
    assert canonicalize({"b": 1, "a": 2}) == '{"a":2,"b":1}'
    assert canonicalize({"x": 1, "y": [3, 2]}) == canonicalize({"y": [3, 2], "x": 1})


def test_jcs_numbers_match_ecmascript():
    # ECMAScript Number.toString (what Rust serde_jcs and JS JSON.stringify use).
    cases = {
        78.0: "78",
        1.0: "1",
        0.003: "0.003",
        -0.0: "0",
        1e-5: "0.00001",
        1e-6: "0.000001",
        1e-7: "1e-7",
        1e20: "100000000000000000000",
        1e21: "1e+21",
        123.456: "123.456",
    }
    for value, expected in cases.items():
        assert canonicalize(value) == expected, f"{value!r} -> {canonicalize(value)}"


def test_jcs_sorts_keys_by_utf16_code_unit():
    # A supplementary-plane key must sort before U+FFFF, matching Rust/JS.
    assert canonicalize({"￿": 1, "\U0001f600": 2}) == '{"\U0001f600":2,"￿":1}'


def test_whole_float_receipt_matches_cross_language_vector():
    # A receipt whose savingsPct and amountUsdc are whole-number floats. The
    # Rust crate, TS SDK, and this package must all hash it to the same digest;
    # regressing Python's number formatting would break receipts silently.
    r = {
        "provider": "blockrun",
        "endpoint": "/v1/chat/completions",
        "modelRequested": "gpt-4o-mini",
        "modelServed": "openai/gpt-4o-mini",
        "verdict": "delivered",
        "inputSha256": "aaaa",
        "outputSha256": "bbbb",
        "routing": {"model": "openai/gpt-4o-mini", "savingsPct": 78.0},
        "payment": {
            "network": "eip155:8453",
            "asset": "0x8335",
            "amount": "1000000",
            "amountUsdc": 1.0,
            "payTo": "0xe903",
            "tx": "0xabc",
        },
    }
    assert (
        canonical_sha256_hex(r)
        == "8328699dcfa1ab8c2d8e5cfca12378999ccf94cffe68fa2ee5cefec818041baf"
    )


def test_decode_live_challenge():
    c = decode_challenge(LIVE_B64)
    a = pick_accept(c, "eip155:8453")
    assert a["amount"] == "3000"
    assert a["payTo"] == "0xe9030014F5DAe217d0A152f02A043567b16c1aBf"


def test_decode_from_www_authenticate():
    c = decode_challenge(f'X402 requirements="{LIVE_B64}"')
    assert len(c["accepts"]) == 1


def test_payment_amount_projects_to_usdc():
    p = payment_from_accept({"amount": "3000"})
    assert abs(p.amount_usdc - 0.003) < 1e-9


def test_delivered_when_served_matches():
    r = build_receipt(
        "/v1/chat/completions",
        {"model": "gpt-4o-mini", "messages": []},
        {"model": "gpt-4o-mini", "choices": []},
        _payment(),
    )
    assert r.verdict == "delivered"


def test_substituted_when_router_swaps():
    r = build_receipt("/x", {"model": "gpt-4o"}, {"model": "gpt-4o-mini"}, _payment())
    assert r.verdict == "substituted"


def test_build_path_digest_matches_literal():
    # Exercises to_json()/RoutingClaim assembly (with a whole-number float
    # savings), not just a hand-built literal, so an assembly regression is caught.
    from covenant_blockrun import RoutingClaim, canonical_sha256_hex

    built = build_receipt(
        "/v1/chat/completions",
        {"model": "gpt-4o-mini", "messages": []},
        {"model": "openai/gpt-4o-mini", "choices": []},
        _payment(),
        RoutingClaim(model="openai/gpt-4o-mini", savings_pct=78.0),
    )
    literal = {
        "provider": "blockrun",
        "endpoint": "/v1/chat/completions",
        "modelRequested": "gpt-4o-mini",
        "modelServed": "openai/gpt-4o-mini",
        "verdict": "delivered",
        "inputSha256": built.input_sha256,
        "outputSha256": built.output_sha256,
        "routing": {"model": "openai/gpt-4o-mini", "savingsPct": 78.0},
        "payment": {
            "network": "eip155:8453",
            "asset": "0x8335",
            "amount": "3000",
            "amountUsdc": 0.003,
            "payTo": "0xe903",
            "tx": "0xabc",
        },
    }
    assert built.digest() == canonical_sha256_hex(literal)


def test_digest_stable_across_key_order():
    a = build_receipt("/x", {"model": "m"}, {"model": "m", "id": "1", "choices": []}, _payment())
    b = build_receipt("/x", {"model": "m"}, {"choices": [], "id": "1", "model": "m"}, _payment())
    assert a.digest() == b.digest()


def test_verify_clean_and_tampered():
    r = build_receipt("/x", {"model": "gpt-4o"}, {"model": "cheap"}, _payment())
    ok = verify_receipt(r, r.digest())
    assert ok["valid"] is True
    r.verdict = "delivered"
    bad = verify_receipt(r)
    assert bad["verdictConsistent"] is False
    assert bad["expectedVerdict"] == "substituted"


# --- transport wrapper (fakes, no httpx dependency) ---


class _Url:
    def __init__(self, url):
        self._url = url
        self.path = "/" + url.split("://", 1)[-1].split("/", 1)[-1] if "://" in url else url

    def __str__(self):
        return self._url


class _Req:
    def __init__(self, method, url, content):
        self.method = method
        self.url = _Url(url)
        self.content = content.encode() if isinstance(content, str) else content


class _Headers(dict):
    def get(self, k, default=None):
        return super().get(k.lower(), default)


class _Stream:
    """A minimal sync httpx byte stream: iterable of chunks with close()."""

    def __init__(self, body: bytes):
        self._chunks = [body] if body else []
        self.closed = False

    def __iter__(self):
        yield from self._chunks

    def close(self):
        self.closed = True


class _Resp:
    def __init__(self, status, headers, body):
        self.status_code = status
        self.headers = _Headers({k.lower(): v for k, v in headers.items()})
        self.stream = _Stream(body.encode() if isinstance(body, str) else body)


class _FakeInner:
    def handle_request(self, request):
        has_payment = request.headers_paid if hasattr(request, "headers_paid") else False
        if not has_payment:
            return _Resp(402, {"x-payment-required": LIVE_B64}, "")
        return _Resp(
            200,
            {"x-payment-receipt": "0xdeadbeef", "x-clawrouter-model": "gpt-4o-mini"},
            '{"model": "gpt-4o-mini", "choices": []}',
        )


def _drain(response):
    """Mimic httpx: read the body, then close the stream (fires the receipt)."""
    body = b"".join(response.stream)
    response.stream.close()
    return body


def test_transport_pairs_402_with_paid_retry():
    received = []
    errors = []
    transport = ReceiptTransport(_FakeInner(), on_receipt=received.append, on_error=errors.append)

    body = '{"model": "gpt-4o-mini", "messages": []}'
    url = "https://blockrun.ai/api/v1/chat/completions"

    first = _Req("POST", url, body)
    _drain(transport.handle_request(first))  # 402, stashes challenge

    retry = _Req("POST", url, body)
    retry.headers_paid = True
    resp = transport.handle_request(retry)
    # The caller's body must still be intact through the tee.
    assert _drain(resp) == b'{"model": "gpt-4o-mini", "choices": []}'

    assert errors == []
    assert len(received) == 1
    r = received[0]
    assert r.verdict == "delivered"
    assert r.payment.tx == "0xdeadbeef"
    assert r.payment.amount == "3000"
    assert r.endpoint == "/api/v1/chat/completions"


def test_transport_skips_non_2xx():
    received = []

    class _ErrInner:
        def handle_request(self, request):
            return _Resp(500, {}, '{"error": "upstream"}')

    transport = ReceiptTransport(_ErrInner(), on_receipt=received.append)
    resp = transport.handle_request(_Req("POST", "https://blockrun.ai/api/v1/x", "{}"))
    _drain(resp)
    assert received == []
