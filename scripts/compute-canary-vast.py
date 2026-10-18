#!/usr/bin/env python3
"""Read-only provider evidence for the Covenant production rental canary.

Never provisions, pays, destroys, runs commands on a machine, or opens SSH.
Only the explicit CLI invocation contacts the provider. Imports and tests are offline.
"""
from __future__ import annotations

import argparse
import base64
import contextlib
import decimal
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import ssl
import stat
import struct
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import uuid

API_ORIGIN = "https://console.vast.ai"
LOG_ORIGIN = "https://s3.amazonaws.com"
MAX_BYTES = 1024 * 1024
PAGE_LIMIT = 1000
MARKER = "COVENANT_SSH_HOST_KEY_V1"



class Refusal(Exception):
    """All messages are fixed strings; never contain URLs, secrets or raw logs."""


def require(condition, message):
    if not condition:
        raise Refusal(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def parse_json(blob):
    try:
        return json.loads(blob, object_pairs_hook=unique_object)
    except (ValueError, UnicodeError):
        raise Refusal("invalid JSON response") from None


def valid_positive_int(value):
    return type(value) is int and 0 < value <= 2**63 - 1


def validate_inputs(job, challenge, host, port):
    try:
        parsed = uuid.UUID(job)
    except (ValueError, AttributeError):
        raise Refusal("job must be a canonical nonnil UUID") from None
    require(str(parsed) == job and parsed.int != 0, "job must be a canonical nonnil UUID")
    require(isinstance(challenge, str) and re.fullmatch(r"[0-9a-f]{64}", challenge)
            and challenge != "0" * 64, "challenge must be nonzero 32-byte lowercase hex")
    require(isinstance(host, str) and 0 < len(host) <= 253 and host.isascii()
            and re.fullmatch(r"[A-Za-z0-9.:-]+", host), "invalid SSH host")
    try:
        ipaddress.ip_address(host)
    except ValueError:
        require(all(re.fullmatch(r"[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?", label)
                    for label in host.split(".")), "invalid SSH host")
    require(type(port) is int and 1 <= port <= 65535, "invalid SSH port")


def validate_log_url(url):
    require(isinstance(url, str) and len(url) <= 512, "untrusted log URL")
    try:
        parsed = urllib.parse.urlsplit(url)
    except ValueError:
        raise Refusal("untrusted log URL") from None
    require(parsed.scheme == "https" and parsed.netloc == "s3.amazonaws.com"
            and parsed.username is None and parsed.password is None
            and not parsed.query and not parsed.fragment
            and (re.fullmatch(r"/vast\.ai/instance_logs/[A-Za-z0-9_-]{1,256}\.log", parsed.path)
                 or re.fullmatch(r"/public\.vast\.ai/instance_logs/[0-9a-f]{64}\.log", parsed.path)),
            "untrusted log URL")
    require(url == LOG_ORIGIN + parsed.path, "noncanonical log URL")
    return url


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Refusal("HTTP redirects are refused")


class Transport:
    def __init__(self):
        # Never forward credentials via environment-supplied HTTP proxies.
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect(),
            urllib.request.HTTPSHandler(context=ssl.create_default_context()))

    def request(self, method, url, headers, data=None):
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self.opener.open(request, timeout=20) as response:
                require(response.status == 200, "unexpected HTTP status")
                require(response.geturl() == url, "response URL changed")
                require(response.headers.get("Content-Encoding", "identity") == "identity",
                        "encoded responses are refused")
                length = response.headers.get("Content-Length")
                if length is not None:
                    require(length.isdigit() and int(length) <= MAX_BYTES, "response exceeds byte limit")
                blob = response.read(MAX_BYTES + 1)
                require(len(blob) <= MAX_BYTES, "response exceeds byte limit")
                return blob
        except urllib.error.HTTPError as error:
            if 300 <= error.code < 400:
                raise Refusal("HTTP redirects are refused") from None
            raise Refusal("provider request returned HTTP " + str(error.code)) from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise Refusal("provider request failed; no pin created") from None


class Provider:
    def __init__(self, token, transport=None):
        require(isinstance(token, str) and 1 <= len(token) <= 4096
                and token.isascii() and not any(c.isspace() for c in token), "invalid provider credential")
        self._token = token
        self.transport = transport or Transport()

    def api(self, method, path, body=None):
        # Provider operations are reads, including search and requesting log copies.
        allowed = ((method == "GET" and (path.startswith("/api/v1/instances/?")
                    or re.fullmatch(r"/api/v0/instances/[1-9][0-9]*/", path)
                    or path == "/api/v0/users/current/"))
                   or (method == "POST" and path == "/api/v0/bundles/")
                   or (method == "PUT" and re.fullmatch(r"/api/v0/instances/request_logs/[1-9][0-9]*", path)))
        require(bool(allowed), "provider mutation refused")
        require(path.startswith("/api/") and "#" not in path, "invalid provider API path")
        headers = {"Authorization": "Bearer " + self._token, "Accept": "application/json",
                   "Accept-Encoding": "identity"}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, separators=(",", ":")).encode()
        return parse_json(self.transport.request(method, API_ORIGIN + path, headers, data))

    def find(self, job):
        label = "covenant-workload-" + job
        query = urllib.parse.urlencode({
            "select_filters": json.dumps({"label": {"eq": label}}, separators=(",", ":")),
            "select_cols": '["id","label"]', "limit": str(PAGE_LIMIT)})
        result = self.api("GET", "/api/v1/instances/?" + query)
        require(isinstance(result, dict) and result.get("success") is True, "instance lookup failed")
        require(result.get("next_token") in (None, ""), "instance list was truncated")
        require(result.get("has_more") in (None, False) and result.get("truncated") in (None, False),
                "instance list was truncated")
        rows = result.get("instances")
        require(isinstance(rows, list) and len(rows) < PAGE_LIMIT, "instance list was truncated or invalid")
        require(len(rows) == 1, "exactly one labelled instance is required")
        row = rows[0]
        require(isinstance(row, dict) and row.get("label") == label and valid_positive_int(row.get("id")),
                "instance label or ID mismatch")
        return row["id"]

    def details(self, instance_id):
        require(valid_positive_int(instance_id), "invalid instance ID")
        result = self.api("GET", "/api/v0/instances/" + str(instance_id) + "/")
        require(isinstance(result, dict) and result.get("success", True) is True,
                "instance detail lookup failed")
        row = result.get("instances")
        require(isinstance(row, dict), "instance details are missing")
        return row

    def logs(self, instance_id):
        require(valid_positive_int(instance_id), "invalid instance ID")
        # daemon_logs deliberately omitted: onstart output is in container logs.
        result = self.api("PUT", "/api/v0/instances/request_logs/" + str(instance_id), {"tail": "1000"})
        require(isinstance(result, dict) and result.get("success") is True, "log request failed")
        url = validate_log_url(result.get("result_url"))
        # A new header set prevents the Vast bearer credential reaching S3.
        blob = self.transport.request("GET", url, {"Accept-Encoding": "identity"})
        require(len(blob) <= MAX_BYTES, "log response exceeds byte limit")
        return blob


def price_micros(value):
    require(type(value) in (int, float, str), "missing or invalid hourly price")
    try:
        amount = decimal.Decimal(str(value))
    except decimal.InvalidOperation:
        raise Refusal("missing or invalid hourly price") from None
    require(amount.is_finite() and amount >= 0 and amount <= 1000, "missing or invalid hourly price")
    return int((amount * 1_000_000).to_integral_value(rounding=decimal.ROUND_CEILING))


def validate_instance(row, instance_id, job, host, port, expected_gpu, maximum):
    require(isinstance(row, dict) and row.get("id") == instance_id
            and valid_positive_int(row.get("id")), "instance ID mismatch")
    require(row.get("label") == "covenant-workload-" + job, "instance label mismatch")
    require(valid_positive_int(row.get("machine_id")), "missing machine ID")
    require(row.get("actual_status") == "running", "instance is not running")
    require(row.get("ssh_host") == host and type(row.get("ssh_port")) is int
            and row["ssh_port"] == port, "provider SSH endpoint differs from lease")
    gpu = row.get("gpu_name")
    require(isinstance(gpu, str) and 1 <= len(gpu) <= 100 and gpu.isascii()
            and all(c.isalnum() or c in " ._+-" for c in gpu), "missing or invalid GPU identity")
    require(expected_gpu is None or gpu == expected_gpu, "GPU identity differs from expected model")
    hourly = price_micros(row.get("dph_total"))
    require(valid_positive_int(maximum) and hourly <= maximum, "hourly price exceeds verification ceiling")
    return {"instanceId": instance_id, "machineId": row["machine_id"], "gpuName": gpu,
            "hourlyUsdMicros": hourly, "sshHost": host, "sshPort": port}


def validate_key(encoded):
    require(isinstance(encoded, str) and len(encoded) == 68, "invalid SSH public key")
    try:
        wire = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error):
        raise Refusal("invalid SSH public key") from None
    require(len(wire) == 51 and wire[:19] == struct.pack(">I", 11) + b"ssh-ed25519" + struct.pack(">I", 32)
            and wire[19:] != bytes(32) and base64.b64encode(wire).decode() == encoded,
            "invalid SSH Ed25519 wire key")
    return "SHA256:" + base64.b64encode(hashlib.sha256(wire).digest()).decode().rstrip("=")


def parse_marker(blob, job, challenge):
    require(isinstance(blob, bytes) and len(blob) <= MAX_BYTES, "log response exceeds byte limit")
    try:
        lines = blob.decode("utf-8", errors="strict").splitlines()
    except UnicodeError:
        raise Refusal("invalid log encoding") from None
    keys = set()
    for line in lines:
        if MARKER not in line:
            continue
        parts = line.split(" ")
        require(len(parts) == 5 and parts[0] == MARKER, "malformed host-key marker")
        require(parts[1] == job, "host-key marker job mismatch")
        require(parts[2] == challenge, "host-key marker challenge mismatch")
        require(parts[3] == "ssh-ed25519", "host-key algorithm mismatch")
        validate_key(parts[4])
        keys.add(parts[4])
    require(len(keys) == 1, "missing or conflicting host-key markers")
    encoded = next(iter(keys))
    return encoded, validate_key(encoded)


def read_private(path, maximum=MAX_BYTES):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                    and info.st_mode & 0o077 == 0, "private file permissions are required")
            require(info.st_size <= maximum, "private file exceeds byte limit")
            blob = stream.read(maximum + 1)
            require(len(blob) <= maximum, "private file exceeds byte limit")
            return blob
    except OSError:
        raise Refusal("private file cannot be read safely") from None


def private_directory(path):
    require(path.is_absolute(), "output directory must be absolute")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0,
            "output directory must be private and not a symlink")


@contextlib.contextmanager
def output_lock(path):
    fd = os.open(path / ".host-key.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_mode & 0o077 == 0,
                "private lock file required")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refusal("another host-key verification is active") from None
        yield
    finally:
        os.close(fd)


def write_once(path, blob):
    if os.path.lexists(path):
        require(read_private(path) == blob, "existing pin or evidence differs; replacement refused")
        return
    fd, temp = tempfile.mkstemp(prefix=".host-key-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(blob)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temp, path)
        except FileExistsError:
            require(read_private(path) == blob, "existing pin or evidence differs; replacement refused")
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        os.unlink(temp)


def known_hosts_line(host, port, key):
    lookup = host if port == 22 else "[" + host + "]:" + str(port)
    return (lookup + " ssh-ed25519 " + key + "\n").encode()


def verify(provider, job, challenge, host, port, output, expected_gpu=None, maximum=1_000_000):
    validate_inputs(job, challenge, host, port)
    require(valid_positive_int(maximum), "invalid hourly price ceiling")
    private_directory(output)
    with output_lock(output):
        instance_id = provider.find(job)
        facts = validate_instance(provider.details(instance_id), instance_id, job, host, port, expected_gpu, maximum)
        key, fingerprint = parse_marker(provider.logs(instance_id), job, challenge)
        # A second authenticated read detects changes during log retrieval.
        later = validate_instance(provider.details(instance_id), instance_id, job, host, port, expected_gpu, maximum)
        require(facts == later, "instance facts changed during verification")
        evidence = {"schema": "covenant-vast-host-key-acceptance-v1", "jobId": job,
                    "challenge": challenge, **facts, "hostKeyAlgorithm": "ssh-ed25519",
                    "hostKey": key, "hostKeyFingerprint": fingerprint,
                    "source": "Authenticated Vast instance lookup and HTTPS provider container logs",
                    "sourceApiOrigin": API_ORIGIN, "sourceLogOrigin": LOG_ORIGIN,
                    "hardwareAttestation": False,
                    "knownHostsFile": "known_hosts"}
        # No log URL, raw logs, credential, command output or SSH private key is persisted.
        encoded = (json.dumps(evidence, indent=2, sort_keys=True) + "\n").encode()
        write_once(output / "host-key-evidence.json", encoded)
        write_once(output / "known_hosts", known_hosts_line(host, port, key))
        return evidence


def canary_challenge(buyer, job):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    require(isinstance(buyer, str) and 32 <= len(buyer) <= 44, "invalid buyer")
    number = 0
    for character in buyer:
        require(character in alphabet, "invalid buyer")
        number = number * 58 + alphabet.index(character)
    raw = bytes(len(buyer) - len(buyer.lstrip("1"))) + number.to_bytes((number.bit_length() + 7) // 8, "big")
    require(len(raw) == 32 and raw != bytes(32), "invalid buyer")
    try:
        identifier = uuid.UUID(job)
    except (ValueError, TypeError, AttributeError):
        raise Refusal("invalid job") from None
    require(str(identifier) == job and identifier.int != 0, "invalid job")
    return hashlib.sha256(b"covenant.compute.canary.host-key.v1\n" + raw + identifier.bytes).hexdigest()


def inventory(provider):
    value = provider.api("GET", "/api/v1/instances/?limit=1000")
    require(isinstance(value, dict) and value.get("success") is True, "inventory unavailable")
    require(value.get("next_token") in (None, "") and value.get("has_more") in (None, False)
            and value.get("truncated") in (None, False), "inventory incomplete")
    rows = value.get("instances")
    require(isinstance(rows, list) and len(rows) < PAGE_LIMIT, "inventory incomplete")
    require(all(isinstance(row, dict) and valid_positive_int(row.get("id")) for row in rows), "inventory invalid")
    require(len({row["id"] for row in rows}) == len(rows), "duplicate provider identity")
    return rows


def config_from_env():
    # Immutable pilot admission bounds. Broad provider credential never appears
    # in this public configuration or in stdout/error diagnostics.
    buyer = os.environ.get("COVENANT_CANARY_BUYER_PUBKEY", "")
    canary_challenge(buyer, "10000000-0000-4000-8000-000000000001")
    directory = Path(os.environ.get("COVENANT_CANARY_STATE_DIR", "/data/rental-canary")) / "provider-evidence"
    private_directory(directory)
    return {"buyer": buyer, "directory": directory, "gpu": "L40S", "max_hourly": 1_000_000,
            "reservation": 350_000, "max_network_per_gb": 4000,
            "coordinator": "https://compute-api.opencovenant.org"}


def preflight(provider, config):
    # Existing unresolved instances block new canary admissions. This is a
    # conservative shared-provider-account fence, not invoice attribution.
    require(not inventory(provider), "provider account has active instances")
    account = provider.api("GET", "/api/v0/users/current/")
    require(price_micros(account.get("credit")) >= config["reservation"], "provider credit insufficient")
    query = {"gpu_name": {"in": [config["gpu"]]}, "num_gpus": {"eq": 1},
             "gpu_ram": {"gte": 40000}, "reliability": {"gte": .99},
             "verified": {"eq": True}, "rentable": {"eq": True}, "rented": {"eq": False},
             "direct_port_count": {"gte": 1}, "disk_space": {"gte": 16}, "allocated_storage": 16,
             "inet_down_cost": {"lte": config["max_network_per_gb"] / 1_000_000},
             "inet_up_cost": {"lte": config["max_network_per_gb"] / 1_000_000},
             "cuda_max_good": {"gte": 12.4}, "gpu_arch": {"eq": "nvidia"},
             "cpu_arch": {"eq": "amd64"}, "type": "ondemand", "order": [["dph_total", "asc"]], "limit": 64}
    offers = provider.api("POST", "/api/v0/bundles/", query).get("offers")
    require(isinstance(offers, list) and len(offers) <= 64, "provider offers unavailable")
    def admitted(row):
        try:
            return (row["gpu_name"] == config["gpu"] and row["num_gpus"] == 1 and row["gpu_ram"] >= 40000
                    and row["verification"] == "verified" and .99 <= row["reliability"] <= 1
                    and row["rentable"] is True and row["rented"] is False
                    and row["direct_port_count"] >= 1 and row["cuda_max_good"] >= 12.4
                    and row["gpu_arch"] == "nvidia" and row["cpu_arch"] == "amd64"
                    and 0 < price_micros(row["dph_total"]) <= config["max_hourly"]
                    and all(price_micros(row[field]) <= config["max_network_per_gb"]
                            for field in ("inet_up_cost", "inet_down_cost")))
        except (KeyError, TypeError, Refusal):
            return False
    require(any(admitted(row) for row in offers if isinstance(row, dict)), "no bounded provider capacity")
    return {"host_key_binding": "authenticated", "cleanup": "provider_readback",
            "provider_limits_checked": True, "provider_reservation_micro_usd": config["reservation"],
            "max_provider_hourly_micro_usd": config["max_hourly"]}


def parse_endpoint(value):
    import shlex
    try:
        parts = shlex.split(value)
        if len(parts) == 4 and parts[0] == "ssh" and parts[2] == "-p":
            user, host = parts[1].split("@", 1)
            port = int(parts[3])
        elif len(parts) == 4 and parts[:2] == ["ssh", "-p"]:
            port = int(parts[2]); user, host = parts[3].split("@", 1)
        else:
            raise ValueError()
        require(user == "root", "unexpected SSH user")
        return host, port, user
    except (ValueError, TypeError):
        raise Refusal("invalid lease SSH endpoint") from None


def adapter(request, provider, config, now):
    require(isinstance(request, dict) and request.get("version") == 1, "invalid adapter request")
    operation = request.get("operation")
    require(operation in {"preflight", "access", "cleanup"}, "unsupported operation")
    require(request.get("coordinator_url") == config["coordinator"], "coordinator binding mismatch")
    result = {"version": 1, "operation": operation}
    if operation == "preflight":
        return result | preflight(provider, config)
    job = request.get("job_id")
    challenge = canary_challenge(config["buyer"], job)
    result.update({"job_id": job, "observed_at_unix": now})
    folder = config["directory"] / job
    private_directory(folder)
    mapping = folder / "instance.json"
    with output_lock(folder):
        known = parse_json(read_private(mapping)) if mapping.exists() else None
        rows = inventory(provider)
        matches = [row for row in rows if row.get("label") == "covenant-workload-" + job]
        require(len(matches) <= 1, "duplicate labelled instances")
        if matches:
            instance = matches[0]["id"]
            proposed = {"job_id": job, "instance_id": instance, "challenge": challenge, "buyer": config["buyer"]}
            write_once(mapping, (json.dumps(proposed, sort_keys=True) + "\n").encode())
            known = proposed
        require(isinstance(known, dict) and known.get("job_id") == job
                and known.get("buyer") == config["buyer"] and known.get("challenge") == challenge
                and valid_positive_int(known.get("instance_id")), "provider mapping unavailable")
        instance = known["instance_id"]
        instance_text = "vast:" + str(instance)
        requested = request.get("provider_instance_id")
        require(requested is None or requested == instance_text, "provider instance substitution")
        if operation == "cleanup":
            # An exact previously authenticated mapping plus a complete fresh
            # inventory proves deletion; a 404/error/unknown mapping does not.
            return result | {"provider_instance_id": instance_text, "binding_verified": True,
                             "source": "provider_readback", "exists": any(row["id"] == instance for row in rows)}
    view = request.get("lease_view")
    require(isinstance(view, dict) and view.get("job_id") == job, "lease view mismatch")
    host, port, user = parse_endpoint(view.get("access", {}).get("endpoint"))
    facts = verify(provider, job, challenge, host, port, folder / "host", config["gpu"], config["max_hourly"])
    require(facts["instanceId"] == instance, "provider mapping changed")
    return result | {"provider_instance_id": instance_text,
                     "binding": {"verified": True, "source": "provider_authenticated", "job_id": job},
                     "ssh": {"host": host, "port": port, "user": user,
                             "public_key": "ssh-ed25519 " + facts["hostKey"]}}


def main():
    import time
    os.umask(0o077)
    try:
        raw = sys.stdin.buffer.read(16_385)
        require(len(raw) <= 16_384, "adapter request exceeds limit")
        config = config_from_env()
        provider = Provider(os.environ.get("COVENANT_CANARY_VAST_API_KEY", ""))
        result = adapter(parse_json(raw), provider, config, int(time.time()))
        print(json.dumps(result, sort_keys=True))
        return 0
    except Refusal as error:
        print(json.dumps({"verified": False, "reason": str(error)}), file=sys.stderr)
        return 1
    except Exception:
        print('{"verified":false,"reason":"provider evidence unavailable"}', file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

