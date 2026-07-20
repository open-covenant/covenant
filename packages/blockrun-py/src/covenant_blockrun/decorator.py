"""The drop-in wedge for Python: an httpx transport that emits a Covenant
receipt for every BlockRun call, without changing how the call is made or how
the money moves.

BlockRun's Python SDK signs x402 payments inside a custom httpx transport
(``AnthropicClient`` wraps ``anthropic.Anthropic`` this way). Wrap that transport
with :class:`ReceiptTransport` and every settled call yields a receipt. The
transport is duck-typed: it only needs a ``handle_request(request) -> response``
method, so it works with httpx sync transports without importing httpx.
"""

from __future__ import annotations

import json
from typing import Any, Callable, Optional

from .challenge import decode_challenge, pick_accept
from .receipt import CallReceipt, PaymentInfo, RoutingClaim, build_receipt, payment_from_accept

OnReceipt = Callable[[CallReceipt], None]

RECEIPT_HEADER = "x-payment-receipt"
REQUIRED_HEADER = "x-payment-required"


class ReceiptTransport:
    """Wrap an httpx-style transport. Pass the result as ``transport=`` (or the
    inner transport of your BlockRun client). ``on_receipt`` is called once per
    completed BlockRun exchange."""

    def __init__(
        self,
        inner: Any,
        on_receipt: OnReceipt,
        *,
        swallow_receipt_errors: bool = True,
    ) -> None:
        self._inner = inner
        self._on_receipt = on_receipt
        self._swallow = swallow_receipt_errors
        # Challenge stash keyed by a request signature, so a 402 pairs with its
        # paid retry.
        self._pending: dict[str, dict[str, Any]] = {}

    def handle_request(self, request: Any) -> Any:
        key = _request_key(request)
        response = self._inner.handle_request(request)
        status = _status(response)

        if status == 402:
            header = _header(response, REQUIRED_HEADER) or _header(response, "www-authenticate")
            if header:
                try:
                    self._pending[key] = decode_challenge(header)
                except Exception:  # noqa: BLE001 - a bad challenge is not fatal
                    pass
            return response

        try:
            self._emit(request, response, key)
        except Exception:  # noqa: BLE001
            if not self._swallow:
                raise
        return response

    def _emit(self, request: Any, response: Any, key: str) -> None:
        req_body = _parse_json(_request_body(request))
        resp_body = _parse_json(_response_body(response))
        routing = _read_routing(response)
        tx = _header(response, RECEIPT_HEADER)
        challenge = self._pending.pop(key, None)
        accept = pick_accept(challenge) if challenge else None
        payment: PaymentInfo = (
            payment_from_accept(accept, tx) if accept else PaymentInfo(tx=tx)
        )
        receipt = build_receipt(
            endpoint=_path(request),
            request=req_body,
            response=resp_body,
            payment=payment,
            routing=routing,
        )
        self._on_receipt(receipt)

    def close(self) -> None:
        close = getattr(self._inner, "close", None)
        if callable(close):
            close()


def _read_routing(response: Any) -> RoutingClaim:
    def first(*names: str) -> Optional[str]:
        for n in names:
            v = _header(response, n)
            if v:
                return v
        return None

    routing = RoutingClaim(
        profile=first("x-clawrouter-profile", "x-blockrun-profile"),
        model=first("x-clawrouter-model", "x-blockrun-model", "x-model"),
    )
    savings = first("x-clawrouter-savings", "x-blockrun-savings")
    if savings:
        try:
            routing.savings_pct = float(savings.rstrip("%").strip())
        except ValueError:
            pass
    return routing


# --- duck-typed httpx accessors (no httpx import required) ---


def _status(response: Any) -> int:
    return int(getattr(response, "status_code", 0))


def _header(obj: Any, name: str) -> Optional[str]:
    headers = getattr(obj, "headers", None)
    if headers is None:
        return None
    try:
        val = headers.get(name)
    except AttributeError:
        return None
    return str(val) if val is not None else None


def _path(request: Any) -> str:
    url = getattr(request, "url", None)
    if url is None:
        return ""
    path = getattr(url, "path", None)
    return str(path) if path is not None else str(url)


def _request_key(request: Any) -> str:
    method = str(getattr(request, "method", "GET"))
    url = str(getattr(request, "url", ""))
    return f"{method} {url} {_request_body(request) or ''}"


def _request_body(request: Any) -> Optional[str]:
    content = getattr(request, "content", None)
    if content is None:
        read = getattr(request, "read", None)
        if callable(read):
            content = read()
    if content is None:
        return None
    if isinstance(content, (bytes, bytearray)):
        return content.decode("utf-8", "replace")
    return str(content)


def _response_body(response: Any) -> Optional[str]:
    # httpx buffers the body on `.content` after `.read()`.
    read = getattr(response, "read", None)
    if callable(read):
        try:
            read()
        except Exception:  # noqa: BLE001
            pass
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray)):
        return content.decode("utf-8", "replace")
    text = getattr(response, "text", None)
    return str(text) if text is not None else None


def _parse_json(text: Optional[str]) -> Any:
    if not text:
        return {}
    try:
        return json.loads(text)
    except (ValueError, TypeError):
        return {}
