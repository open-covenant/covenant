#!/usr/bin/env python3
"""Bounded outbound relay wire protocol; no attestation trust is granted here."""
import base64
import hashlib
import http.client
import ipaddress
import json
import re
import ssl
import socket
import threading
import time
import urllib.parse
import uuid

VERSION = 1
MAX_HTTP = 768 * 1024
MAX_CHUNK = 16 * 1024
MAX_BUFFER = 256 * 1024
MAX_STREAM_BYTES = 16 * 1024 * 1024
MAX_STREAM_SECONDS = 300
MAX_OPERATIONS = 16
HTTP_SECONDS = 12


class ProtocolError(ValueError):
    def __init__(self, code, status=409):
        super().__init__(code)
        self.code, self.status = code, status


def require(condition, code, status=409):
    if not condition:
        raise ProtocolError(code, status)


def fields(value, names):
    require(type(value) is dict and set(value) == set(names), "unexpected_fields", 400)


def integer(value, low=0, high=(1 << 53) - 1):
    require(type(value) is int and low <= value <= high, "invalid_integer", 400)
    return value


def hex_string(value, length):
    require(type(value) is str and re.fullmatch(r"[0-9a-f]{%d}" % length, value), "invalid_hex", 400)
    return value


def uuid_string(value):
    require(type(value) is str and len(value) == 36, "invalid_uuid", 400)
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise ProtocolError("invalid_uuid", 400) from error
    require(parsed.int != 0 and str(parsed) == value, "invalid_uuid", 400)
    return value


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, "duplicate_field", 400)
            result[key] = value
        return result

    def invalid(_):
        raise ProtocolError("noninteger_number", 400)

    try:
        return json.loads(raw, object_pairs_hook=pairs, parse_float=invalid, parse_constant=invalid)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ProtocolError("invalid_json", 400) from error


def encoded(value):
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()
    require(len(raw) <= MAX_HTTP, "body_too_large", 413)
    return raw


def fingerprint(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def decode_chunk(value):
    require(type(value) is str and len(value) <= ((MAX_CHUNK + 2) // 3) * 4,
            "chunk_too_large", 413)
    try:
        raw = base64.b64decode(value, validate=True)
    except ValueError as error:
        raise ProtocolError("invalid_base64", 400) from error
    require(len(raw) <= MAX_CHUNK and base64.b64encode(raw).decode() == value,
            "invalid_chunk", 400)
    return raw


def now_ms():
    return time.time_ns() // 1_000_000


def endpoint_origin(value, allow_loopback=False):
    require(type(value) is str and len(value) <= 512, "invalid_endpoint", 400)
    parsed = urllib.parse.urlsplit(value)
    require(not parsed.username and not parsed.password and not parsed.query
            and not parsed.fragment and parsed.path in ("", "/"), "invalid_endpoint", 400)
    host = parsed.hostname
    require(host is not None and host == host.lower(), "invalid_endpoint", 400)
    if allow_loopback and parsed.scheme == "http":
        try:
            loopback = ipaddress.ip_address(host).is_loopback
        except ValueError:
            loopback = False
        require(loopback and parsed.port is not None, "loopback_http_only", 400)
        return parsed.scheme, host, integer(parsed.port, 1, 65535)
    require(parsed.scheme == "https" and parsed.port in (None, 443)
            and re.fullmatch(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+"
                             r"[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?", host),
            "https_dns_origin_required", 400)
    return "https", host, 443


class HttpTransport:
    """One attempt per call; callers reconcile using their original operation IDs."""
    def __init__(self, endpoint, session_id, token=None, allow_loopback=False):
        self.scheme, self.host, self.port = endpoint_origin(endpoint, allow_loopback)
        self.session_id = uuid_string(session_id)
        self.token = token
        if token is not None:
            hex_string(token, 64)

    def request(self, operation, arguments, token=None):
        body = encoded({"version": VERSION, "session_id": self.session_id,
                        "operation": operation, "arguments": arguments})
        if self.scheme == "https":
            context = ssl.create_default_context()
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            connection = http.client.HTTPSConnection(self.host, self.port, timeout=HTTP_SECONDS, context=context)
        else:
            connection = http.client.HTTPConnection(self.host, self.port, timeout=HTTP_SECONDS)
        headers = {"Content-Type": "application/json", "Accept-Encoding": "identity",
                   "Connection": "close", "User-Agent": "Covenant-Compute-Outbound/1"}
        credential = self.token if token is None else token
        if credential is not None:
            hex_string(credential, 64)
            headers["Authorization"] = "Bearer " + credential
        timed_out = threading.Event()
        held_socket = [None]

        def cut_connection():
            timed_out.set()
            active = connection.sock or held_socket[0]
            if active is not None:
                try:
                    active.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

        timer = threading.Timer(HTTP_SECONDS, cut_connection)
        timer.daemon = True
        timer.start()
        try:
            connection.request("POST", "/v1/relay", body=body, headers=headers)
            held_socket[0] = connection.sock
            require(not timed_out.is_set(), "http_deadline_expired", 504)
            response = connection.getresponse()
            require(response.getheader("Content-Encoding", "identity") == "identity",
                    "unexpected_encoding", 502)
            raw = response.read(MAX_HTTP + 1)
            require(not timed_out.is_set(), "http_deadline_expired", 504)
            require(len(raw) <= MAX_HTTP, "relay_response_too_large", 502)
            value = strict_json(raw)
            if response.status != 200:
                code = value.get("error", "relay_rejected") if type(value) is dict else "relay_rejected"
                require(type(code) is str and re.fullmatch(r"[a-z_]{1,80}", code), "relay_rejected", 502)
                raise ProtocolError(code, response.status)
            fields(value, {"version", "result"})
            require(value["version"] == VERSION and type(value["version"]) is int,
                    "wrong_protocol", 502)
            return value["result"]
        finally:
            timer.cancel()
            connection.close()
