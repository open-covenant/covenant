"""The drop-in wedge for Python: an httpx transport that emits a Covenant receipt
for every settled BlockRun call, without changing how the call is made or how the
money moves.

BlockRun's Python SDK signs x402 payments inside a custom httpx transport
(``AnthropicClient`` wraps ``anthropic.Anthropic`` this way). Wrap that transport
with :class:`ReceiptTransport` and every settled call yields a receipt. Both the
sync (``handle_request``) and async (``handle_async_request``) paths are handled.

A transport must not consume the caller's response body, so the response stream is
teed: bytes flow to the caller unchanged while a bounded copy is accumulated, and
the receipt is built when the stream closes. Streaming responses that exceed the
copy cap still pass through; their receipt simply carries no output body.
"""

from __future__ import annotations

import json
from typing import Any, Callable, Optional

from .challenge import decode_challenge, pick_accept
from .receipt import CallReceipt, PaymentInfo, RoutingClaim, build_receipt, payment_from_accept

OnReceipt = Callable[[CallReceipt], None]
OnError = Callable[[BaseException], None]

RECEIPT_HEADER = "x-payment-receipt"
REQUIRED_HEADER = "x-payment-required"

# Cap on outstanding 402 challenges awaiting a paid retry, and on the response
# bytes copied per call for the receipt.
MAX_PENDING = 1024
MAX_BODY_COPY = 16 * 1024 * 1024


class ReceiptTransport:
    """Wrap an httpx-style transport. Pass the result as ``transport=`` (or the
    inner transport of your BlockRun client). ``on_receipt`` is called once per
    settled BlockRun exchange; ``on_error``, if given, receives any exception
    from building or delivering a receipt (these never affect the call)."""

    def __init__(
        self,
        inner: Any,
        on_receipt: OnReceipt,
        *,
        on_error: Optional[OnError] = None,
    ) -> None:
        self._inner = inner
        self._on_receipt = on_receipt
        self._on_error = on_error
        self._pending: dict[str, dict[str, Any]] = {}

    def handle_request(self, request: Any) -> Any:
        key = _request_key(request)
        response = self._inner.handle_request(request)
        challenge = self._route(request, response, key)
        if challenge is _SKIP:
            return response
        response.stream = _TeeStream(
            response.stream, self._emit_cb(request, response, challenge)
        )
        return response

    async def handle_async_request(self, request: Any) -> Any:
        key = _request_key(request)
        response = await self._inner.handle_async_request(request)
        challenge = self._route(request, response, key)
        if challenge is _SKIP:
            return response
        response.stream = _AsyncTeeStream(
            response.stream, self._emit_cb(request, response, challenge)
        )
        return response

    def _route(self, request: Any, response: Any, key: str) -> Any:
        """Stash a 402, drop the paired/failed challenge otherwise, and decide
        whether this response should produce a receipt. Returns the matched
        challenge (or None), or the ``_SKIP`` sentinel to pass through untouched."""
        status = _status(response)
        if status == 402:
            header = _header(response, REQUIRED_HEADER) or _header(response, "www-authenticate")
            if header:
                try:
                    if len(self._pending) >= MAX_PENDING:
                        self._pending.pop(next(iter(self._pending)))
                    self._pending[key] = decode_challenge(header)
                except Exception as err:  # noqa: BLE001
                    self._report(err)
            return _SKIP
        challenge = self._pending.pop(key, None)
        if not (200 <= status < 300):
            return _SKIP
        return challenge

    def _emit_cb(self, request: Any, response: Any, challenge: Any) -> Callable[[bytes], None]:
        def done(body: bytes) -> None:
            try:
                self._emit(request, response, challenge, body)
            except Exception as err:  # noqa: BLE001
                self._report(err)

        return done

    def _emit(self, request: Any, response: Any, challenge: Any, body: bytes) -> None:
        req_body = _parse_json(_request_body(request))
        resp_body = _parse_json(body.decode("utf-8", "replace")) if body else {}
        routing = _read_routing(response)
        tx = _header(response, RECEIPT_HEADER)
        accept = pick_accept(challenge) if challenge else None
        payment: PaymentInfo = payment_from_accept(accept, tx) if accept else PaymentInfo(tx=tx)
        receipt = build_receipt(
            endpoint=_path(request),
            request=req_body,
            response=resp_body,
            payment=payment,
            routing=routing,
        )
        self._on_receipt(receipt)

    def _report(self, err: BaseException) -> None:
        if self._on_error is not None:
            self._on_error(err)

    def close(self) -> None:
        close = getattr(self._inner, "close", None)
        if callable(close):
            close()

    async def aclose(self) -> None:
        aclose = getattr(self._inner, "aclose", None)
        if callable(aclose):
            await aclose()


_SKIP = object()


class _TeeStream:
    """A sync httpx byte stream that copies (up to a cap) what flows through and
    runs ``on_done`` with the copy when the stream closes. Passing bytes straight
    through keeps the caller's body intact."""

    def __init__(self, inner: Any, on_done: Callable[[bytes], None]) -> None:
        self._inner = inner
        self._on_done = on_done
        self._buf = bytearray()
        self._truncated = False
        self._fired = False

    def __iter__(self):
        for chunk in self._inner:
            self._accumulate(chunk)
            yield chunk

    def _accumulate(self, chunk: bytes) -> None:
        if not self._truncated and len(self._buf) + len(chunk) <= MAX_BODY_COPY:
            self._buf += chunk
        else:
            self._truncated = True

    def close(self) -> None:
        inner_close = getattr(self._inner, "close", None)
        if callable(inner_close):
            inner_close()
        self._fire()

    def _fire(self) -> None:
        if self._fired:
            return
        self._fired = True
        self._on_done(b"" if self._truncated else bytes(self._buf))


class _AsyncTeeStream:
    """Async counterpart of :class:`_TeeStream`."""

    def __init__(self, inner: Any, on_done: Callable[[bytes], None]) -> None:
        self._inner = inner
        self._on_done = on_done
        self._buf = bytearray()
        self._truncated = False
        self._fired = False

    async def __aiter__(self):
        async for chunk in self._inner:
            if not self._truncated and len(self._buf) + len(chunk) <= MAX_BODY_COPY:
                self._buf += chunk
            else:
                self._truncated = True
            yield chunk

    async def aclose(self) -> None:
        inner_aclose = getattr(self._inner, "aclose", None)
        if callable(inner_aclose):
            await inner_aclose()
        if not self._fired:
            self._fired = True
            self._on_done(b"" if self._truncated else bytes(self._buf))


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


# duck-typed httpx accessors, so httpx need not be imported


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


def _parse_json(text: Optional[str]) -> Any:
    if not text:
        return {}
    try:
        return json.loads(text)
    except (ValueError, TypeError):
        return {}
