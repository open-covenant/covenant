#!/usr/bin/env python3
"""Exercise a remote rental through the installed buyer CLI, never a local engine.

This deliberately has no payment-key or provider mutation implementation. Funding
is an explicit, already-broadcast deposit claim. The trusted evidence adapter is
read-only; see deploy/compute-canary/README.md for its contract and trust boundary.
"""

import argparse
import base64
import csv
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
import uuid


WINDOW_SECONDS = 6 * 60 * 60
DAY_SECONDS = 24 * 60 * 60
DAILY_LIMIT = 2_000_000
PILOT_LIMIT = 20_000_000
HISTORY_LIMIT = 40
PRIVATE_RUN_LIMIT = 1024
TERMINAL = {"completed", "failed", "refunded", "rejected", "cancelled"}
GPU_PROBE = "nvidia-smi --query-gpu=uuid,name,memory.total --format=csv,noheader,nounits"
JOB_IN_STDERR = re.compile(r"lease ([0-9a-f-]{36}) opened —")


class CanaryError(Exception):
    def __init__(self, code, verdict="failed"):
        super().__init__(code)
        self.code = code
        self.verdict = verdict


def integer(value, name, minimum=0, maximum=2**63 - 1):
    if type(value) is not int or not minimum <= value <= maximum:
        raise CanaryError("invalid_" + name, "blocked")
    return value


def command(value, name):
    if not isinstance(value, list) or not value or any(
        not isinstance(item, str) or not item or "\0" in item for item in value
    ):
        raise CanaryError("invalid_" + name, "blocked")
    return value


def atomic_json(path, value, mode=0o600):
    path = Path(path)
    fd, temporary = tempfile.mkstemp(prefix="." + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            os.fchmod(stream.fileno(), mode)
            json.dump(value, stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def read_json(path):
    with open(path, encoding="utf-8") as stream:
        return json.load(stream)


def remote_url(value, name):
    parsed = urlparse(value if isinstance(value, str) else "")
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise CanaryError("invalid_" + name, "blocked")
    # Local fake processes in tests still receive a remote-looking configured
    # URL. Production never starts an in-process coordinator or mock RPC.
    if parsed.hostname.lower() in {"localhost", "localhost.localdomain"}:
        raise CanaryError("local_" + name, "blocked")
    try:
        if not ipaddress.ip_address(parsed.hostname).is_global:
            raise CanaryError("local_" + name, "blocked")
    except ValueError:
        pass
    return value


def load_config(path):
    config = read_json(path)
    if not isinstance(config, dict):
        raise CanaryError("invalid_config", "blocked")
    for name in ("coordinator_url", "rpc_url"):
        config[name] = remote_url(config.get(name), name)
    config["cli"] = command(config.get("cli", ["covenant-compute"]), "cli")
    config["evidence_command"] = command(config.get("evidence_command"), "evidence_command")
    if config.get("alert_command") is not None:
        config["alert_command"] = command(config["alert_command"], "alert_command")
    for name in ("buyer_home", "ssh_private_key", "ssh_public_key"):
        value = config.get(name)
        if not isinstance(value, str) or not Path(value).is_absolute():
            raise CanaryError("invalid_" + name, "blocked")
    # No implicit authorization to spend. A deployment must name its ceiling
    # and per-second offer; no automatic price escalation is implemented.
    for name in ("max_spend_micro_usdc", "rate_micro_usdc_per_sec"):
        config[name] = integer(config.get(name), name, 1)
    for name, ceiling in (("daily_limit_micro_usd", DAILY_LIMIT), ("pilot_limit_micro_usd", PILOT_LIMIT)):
        config[name] = integer(config.get(name), name, 1, ceiling)
    for name in ("provider_reservation_micro_usd", "fee_reservation_micro_usd", "storage_reservation_micro_usd"):
        config[name] = integer(config.get(name), name, 1)
    if config["provider_reservation_micro_usd"] < 350_000 or config["fee_reservation_micro_usd"] < 20_000 or config["storage_reservation_micro_usd"] < 1_000:
        raise CanaryError("operating_reservation_too_small", "blocked")
    config["min_gpu_memory_mib"] = integer(config.get("min_gpu_memory_mib"), "min_gpu_memory_mib", 1, 1048576)
    if not isinstance(config.get("expected_gpu_model"), str) or not re.fullmatch(r"[A-Za-z0-9 ._()+/-]{1,80}", config["expected_gpu_model"]):
        raise CanaryError("invalid_expected_gpu_model", "blocked")
    if not isinstance(config.get("expected_cli_version"), str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", config["expected_cli_version"]):
        raise CanaryError("invalid_expected_cli_version", "blocked")
    if not isinstance(config.get("public_state_path"), str) or not Path(config["public_state_path"]).is_absolute():
        raise CanaryError("invalid_public_state_path", "blocked")
    defaults = {"duration_secs": 300, "readiness_timeout_secs": 180,
                "settlement_timeout_secs": 180, "cleanup_timeout_secs": 180,
                "command_timeout_secs": 45, "poll_secs": 5, "min_vram_gb": 1}
    for name, default in defaults.items():
        config[name] = integer(config.get(name, default), name, 1, 3600)
    if config["readiness_timeout_secs"] >= config["duration_secs"]:
        raise CanaryError("readiness_exceeds_lease_window", "blocked")
    ceiling = config["rate_micro_usdc_per_sec"] * config["duration_secs"]
    if ceiling > config["max_spend_micro_usdc"]:
        raise CanaryError("lease_exceeds_max_spend", "blocked")
    config["lease_ceiling"] = ceiling
    config["operating_ceiling"] = ceiling + sum(config[name] for name in
        ("provider_reservation_micro_usd", "fee_reservation_micro_usd", "storage_reservation_micro_usd"))
    if config["operating_ceiling"] > min(config["daily_limit_micro_usd"], config["pilot_limit_micro_usd"]):
        raise CanaryError("run_exceeds_operating_budget", "blocked")
    for name in ("ssh_private_key", "ssh_public_key"):
        if not Path(config[name]).is_file():
            raise CanaryError("missing_" + name, "blocked")
    if not Path(config["buyer_home"]).is_dir():
        raise CanaryError("missing_buyer_home", "blocked")
    if config.get("deposit_signature") is not None and not re.fullmatch(
        r"[1-9A-HJ-NP-Za-km-z]{64,100}", str(config["deposit_signature"])
    ):
        raise CanaryError("invalid_deposit_signature", "blocked")
    return config


class Budget:
    """One atomic journal is the source of truth for reservations and recovery.

    run.lock must be held while mutating. Unresolved entries never age out of
    the rolling window. Resolved entries count from their final reconciliation,
    conservatively covering delayed charges. Provider/fee/storage allocations
    are never refunded on the strength of a lower buyer charge.
    """
    def __init__(self, directory, config):
        self.path, self.config = directory / "budget.json", config
        try:
            fd = os.open(Path(config["buyer_home"]) / "identity.json", os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_size != 32:
                    raise ValueError()
                seed = stream.read(33)
                if len(seed) != 32:
                    raise ValueError()
                fingerprint = hashlib.sha256(seed).hexdigest()
                del seed
        except (OSError, ValueError):
            raise CanaryError("invalid_buyer_identity", "blocked") from None
        self.identity = {"coordinator_url": config["coordinator_url"], "buyer_home": str(Path(config["buyer_home"]).resolve()),
                         "buyer_identity_sha256": fingerprint,
                         "daily_limit_micro_usd": config["daily_limit_micro_usd"], "pilot_limit_micro_usd": config["pilot_limit_micro_usd"]}
        if self.path.exists():
            self.value = read_json(self.path)
            if self.value.get("schema") != 1 or self.value.get("identity") != self.identity:
                raise CanaryError("budget_identity_or_limits_changed", "blocked")
            self.validate()
        else:
            # A missing monetary journal beside prior runs must never reset a pilot.
            if (directory / "budget-initialized").exists() or (directory / "active.json").exists():
                raise CanaryError("budget_journal_missing", "blocked")
            self.value = {"schema": 1, "identity": self.identity, "created_at_unix": int(time.time()), "entries": [], "latched_reason": None}
            self.save()
            atomic_json(directory / "budget-initialized", {"schema": 1})

    def validate(self):
        entries = self.value.get("entries")
        if not isinstance(entries, list) or len(entries) > 128:
            raise CanaryError("invalid_budget_journal", "blocked")
        ids = set()
        for entry in entries:
            job = entry.get("job_id")
            if not isinstance(job, str) or str(uuid.UUID(job)) != job or job in ids:
                raise CanaryError("invalid_budget_journal", "blocked")
            ids.add(job)
            for name in ("reserved_at_unix", "buyer_reserved_micro_usdc", "provider_reserved_micro_usd", "fee_reserved_micro_usd", "storage_reserved_micro_usd", "counted_micro_usd"):
                integer(entry.get(name), "budget_" + name)
            if entry.get("resolved_at_unix") is not None:
                integer(entry["resolved_at_unix"], "resolved_at_unix")
            floor = entry["provider_reserved_micro_usd"] + entry["fee_reserved_micro_usd"] + entry["storage_reserved_micro_usd"]
            if entry["counted_micro_usd"] < floor or (entry.get("resolved_at_unix") is None and entry["counted_micro_usd"] < floor + entry["buyer_reserved_micro_usdc"]):
                raise CanaryError("invalid_budget_journal", "blocked")
        if sum(entry.get("resolved_at_unix") is None for entry in entries) > 1:
            raise CanaryError("multiple_unresolved_rentals", "blocked")

    def save(self):
        atomic_json(self.path, self.value)

    def totals(self, now=None):
        now = int(time.time()) if now is None else now
        entries = self.value["entries"]
        return {"daily_limit_micro_usd": self.identity["daily_limit_micro_usd"], "pilot_limit_micro_usd": self.identity["pilot_limit_micro_usd"],
                "daily_reserved_micro_usd": sum(e["counted_micro_usd"] for e in entries if e.get("resolved_at_unix") is None or e["resolved_at_unix"] > now - DAY_SECONDS),
                "pilot_reserved_micro_usd": sum(e["counted_micro_usd"] for e in entries)}

    def pause_reason(self, now=None):
        if self.value.get("latched_reason"):
            return self.value["latched_reason"]
        if any(e.get("resolved_at_unix") is None for e in self.value["entries"]):
            return "unresolved_rental"
        totals = self.totals(now)
        if totals["pilot_reserved_micro_usd"] + self.config["operating_ceiling"] > totals["pilot_limit_micro_usd"]:
            return "pilot_budget_exhausted"
        if totals["daily_reserved_micro_usd"] + self.config["operating_ceiling"] > totals["daily_limit_micro_usd"]:
            return "daily_budget_exhausted"
        return None

    def reserve(self, active, now=None):
        now = int(time.time()) if now is None else now
        reason = self.pause_reason(now)
        if reason:
            raise CanaryError(reason, "blocked")
        entries = self.value["entries"]
        if len(entries) >= 128:
            raise CanaryError("pilot_entry_limit", "blocked")
        if entries and now // WINDOW_SECONDS <= max(e["reserved_at_unix"] // WINDOW_SECONDS for e in entries):
            raise CanaryError("window_already_reserved", "blocked")
        entry = {"job_id": active["job_id"], "run_id": active["run_id"], "reserved_at_unix": now,
                 "buyer_reserved_micro_usdc": self.config["lease_ceiling"],
                 "provider_reserved_micro_usd": self.config["provider_reservation_micro_usd"],
                 "fee_reserved_micro_usd": self.config["fee_reservation_micro_usd"],
                 "storage_reserved_micro_usd": self.config["storage_reservation_micro_usd"],
                 "counted_micro_usd": self.config["operating_ceiling"], "resolved_at_unix": None, "active": active}
        entries.append(entry)
        self.save()  # contains the complete recovery record BEFORE any paid command

    def active(self):
        return next((e["active"] for e in self.value["entries"] if e.get("resolved_at_unix") is None), None)

    def update_active(self, active):
        entry = next(e for e in self.value["entries"] if e["job_id"] == active["job_id"])
        entry["active"] = active
        self.save()

    def resolve(self, active, charged, provider_cost=None):
        entry = next(e for e in self.value["entries"] if e["job_id"] == active["job_id"])
        if provider_cost is not None and provider_cost > entry["provider_reserved_micro_usd"]:
            entry["provider_reserved_micro_usd"] = provider_cost
            self.value["latched_reason"] = "provider_budget_breach"
        entry["counted_micro_usd"] = charged + sum(entry[name] for name in ("provider_reserved_micro_usd", "fee_reserved_micro_usd", "storage_reserved_micro_usd"))
        entry["resolved_at_unix"] = int(time.time())
        entry["active"] = active
        self.save()

    def observe_provider_cost(self, active, amount):
        entry = next(e for e in self.value["entries"] if e["job_id"] == active["job_id"])
        if amount > entry["provider_reserved_micro_usd"]:
            entry["counted_micro_usd"] += amount - entry["provider_reserved_micro_usd"]
            entry["provider_reserved_micro_usd"] = amount
            self.value["latched_reason"] = "provider_budget_breach"
            self.save()

    def record_success(self, record):
        self.value["last_success_at_unix"] = record["finished_at_unix"]
        self.value["last_success_window"] = record["started_at_unix"] // WINDOW_SECONDS
        self.save()


class Canary:
    def __init__(self, directory, config, record):
        self.directory, self.config, self.record = directory, config, record
        self.active_path = directory / "active.json"
        self.active = None
        self.budget = Budget(directory, config)
        self.env = dict(os.environ)
        self.env.update({
            "COVENANT_COMPUTE_COORDINATOR_URL": config["coordinator_url"],
            "COVENANT_COMPUTE_RPC_URL": config["rpc_url"],
            "COVENANT_COMPUTE_MCP_HOME": config["buyer_home"],
            "COVENANT_COMPUTE_MAX_PRICE_MICRO_USDC": str(config["max_spend_micro_usdc"]),
            "COVENANT_COMPUTE_DAILY_BUDGET_MICRO_USDC": str(config["daily_limit_micro_usd"]),
        })

    def checkpoint(self, stage):
        self.record["stage"] = stage
        atomic_json(self.directory / "runs" / (self.record["run_id"] + ".json"), self.record)
        publish_public_state(self.directory, self.config)

    def run_process(self, argv, payload=None):
        # No shell, no credentials in diagnostics or persisted stdout/stderr.
        try:
            result = subprocess.run(argv, input=None if payload is None else json.dumps(payload),
                                    text=True, capture_output=True, env=self.env,
                                    timeout=self.config["command_timeout_secs"], check=False)
        except subprocess.TimeoutExpired as error:
            return -1, self.text(error.stdout), self.text(error.stderr)
        except OSError:
            raise CanaryError("subprocess_unavailable", "blocked") from None
        if len(result.stdout) + len(result.stderr) > 1024 * 1024:
            raise CanaryError("subprocess_output_limit")
        return result.returncode, result.stdout, result.stderr

    @staticmethod
    def text(value):
        return value.decode("utf-8", "replace") if isinstance(value, bytes) else (value or "")

    def json_process(self, argv, payload=None):
        code, output, _ = self.run_process(argv, payload)
        if code != 0:
            raise CanaryError("subprocess_timeout" if code == -1 else "subprocess_failed")
        try:
            return json.loads(output)
        except ValueError:
            raise CanaryError("invalid_subprocess_json") from None

    def cli(self, *arguments):
        return self.json_process(self.config["cli"] + list(arguments) + ["--json"])

    def evidence(self, operation, view=None):
        request = {"version": 1, "operation": operation, "run_id": self.record["run_id"],
                   "coordinator_url": self.config["coordinator_url"]}
        if self.active:
            request.update({"job_id": self.active.get("job_id"),
                            "provider_instance_id": self.active.get("provider_instance_id"),
                            "lease_view": view})
        evidence = self.json_process(self.config["evidence_command"], request)
        if not isinstance(evidence, dict) or evidence.get("version") != 1 or evidence.get("operation") != operation:
            raise CanaryError("invalid_evidence", "blocked")
        if operation != "preflight":
            if evidence.get("job_id") != self.active.get("job_id"):
                raise CanaryError("evidence_job_mismatch", "blocked")
            observed = evidence.get("observed_at_unix")
            if type(observed) not in (int, float) or not time.time() - 90 <= observed <= time.time() + 5:
                raise CanaryError("stale_evidence", "blocked")
        return evidence

    def save_active(self):
        self.budget.update_active(self.active)
        atomic_json(self.active_path, self.active)

    def balance(self):
        value = self.cli("balance").get("balance", {})
        return {name: integer(value.get(name), name) for name in
                ("available_micro_usdc", "charged_micro_usdc", "deposited_micro_usdc", "withdrawn_micro_usdc")}

    def preflight(self):
        self.checkpoint("preflight")
        code, version, _ = self.run_process(self.config["cli"] + ["--version"])
        if code or version.strip() != "covenant-compute " + self.config["expected_cli_version"]:
            raise CanaryError("buyer_version_unavailable", "blocked")
        self.record["buyer_version"] = version.strip()[:100]
        capacity = self.cli("capacity")
        candidates = [row for row in capacity.get("entries", [])
                      if row.get("kind") == "lease_session" and row.get("operators", 0) > 0
                      and row.get("max_vram_gb", 0) >= self.config["min_vram_gb"]
                      and row.get("min_ask_unit") in {"per_gpu_second", "per_lease_hour"}
                      and (not self.config.get("gpu_class") or self.config["gpu_class"] in row.get("gpu_classes", []))]
        candidates = [row for row in candidates if
                      row.get("min_ask_micro_usdc", 2**63) <= self.config["rate_micro_usdc_per_sec"] *
                      (3600 if row["min_ask_unit"] == "per_lease_hour" else 1)]
        if not candidates:
            raise CanaryError("no_affordable_gpu_capacity", "blocked")
        self.record["capacity_candidates"] = len(candidates)
        support = self.evidence("preflight")
        if support.get("host_key_binding") != "authenticated" or support.get("cleanup") != "provider_readback":
            raise CanaryError("authenticated_evidence_unavailable", "blocked")
        if support.get("provider_limits_checked") is not True or support.get("provider_reservation_micro_usd") != self.config["provider_reservation_micro_usd"]:
            raise CanaryError("provider_limits_not_checked", "blocked")
        self.record["balance_before"] = self.balance()

    def open_lease(self):
        self.checkpoint("funding")
        balance = self.record["balance_before"]
        if balance["available_micro_usdc"] < self.config["lease_ceiling"]:
            signature = self.config.get("deposit_signature")
            if not signature:
                raise CanaryError("insufficient_funds", "blocked")
            self.cli("deposit", signature)  # claims only this explicitly named existing transfer
            balance = self.balance()
            self.record["balance_before"] = balance
            if balance["available_micro_usdc"] < self.config["lease_ceiling"]:
                raise CanaryError("insufficient_funds_after_deposit", "blocked")
        job_id = str(uuid.uuid4())
        active = {"run_id": self.record["run_id"], "job_id": job_id,
                       "coordinator_url": self.config["coordinator_url"],
                       "buyer_home": self.config["buyer_home"], "launch_started_unix": time.time(),
                       "lease_ceiling": self.config["lease_ceiling"], "balance_before": balance,
                       "launch_pending": True}
        self.budget.reserve(active)
        self.active = active
        self.record["job_id"] = job_id
        self.save_active()
        self.checkpoint("launch")
        args = ["lease", "open", "--job-id", job_id, "--duration-secs", str(self.config["duration_secs"]),
                "--rate", str(self.config["rate_micro_usdc_per_sec"]), "--ssh-key", self.config["ssh_public_key"],
                "--min-vram-gb", str(self.config["min_vram_gb"]), "--no-wait", "--json"]
        if self.config.get("gpu_class"):
            args += ["--gpu-class", self.config["gpu_class"]]
        code, output, error = self.run_process(self.config["cli"] + args)
        try:
            response_job_id = str(uuid.UUID(json.loads(output)["job_id"]))
        except (ValueError, KeyError, TypeError):
            match = JOB_IN_STDERR.search(error)
            response_job_id = str(uuid.UUID(match.group(1))) if match else None
        if response_job_id and response_job_id != job_id:
            raise CanaryError("launch_job_mismatch", "blocked")
        if response_job_id:
            self.active.update({"launch_pending": False})
            self.save_active()
        if not response_job_id:
            raise CanaryError("launch_outcome_unknown_do_not_retry", "blocked")
        if code:
            raise CanaryError("launch_response_failed")

    def poll(self, timeout, operation):
        end = time.monotonic() + timeout
        while True:
            value = operation()
            if value is not None:
                return value
            if time.monotonic() >= end:
                raise CanaryError("stage_timeout")
            time.sleep(min(self.config["poll_secs"], max(0, end - time.monotonic())))

    def view(self):
        value = self.cli("lease", "view", self.active["job_id"])
        if value.get("job_id") != self.active["job_id"]:
            raise CanaryError("lease_job_mismatch")
        return value

    def wait_ready(self):
        self.checkpoint("ready")
        def check():
            view = self.view()
            if view.get("status") in TERMINAL:
                raise CanaryError("lease_ended_before_probe")
            return view if view.get("access") else None
        return self.poll(self.config["readiness_timeout_secs"], check)

    def probe(self, view):
        self.checkpoint("ssh_identity")
        evidence = self.evidence("access", view)
        instance = evidence.get("provider_instance_id")
        if not isinstance(instance, str) or not instance:
            raise CanaryError("missing_provider_identity", "blocked")
        # Keep provider identity even when later host validation fails, so
        # teardown can still prove this exact machine no longer bills.
        self.active["provider_instance_id"] = instance
        self.save_active()
        binding = evidence.get("binding", {})
        if binding.get("verified") is not True or binding.get("source") not in {
            "operator_signed_ready", "provider_authenticated"
        } or binding.get("job_id") != self.active["job_id"]:
            raise CanaryError("host_key_binding_unverified", "blocked")
        host = evidence.get("ssh", {})
        hostname, port, username = endpoint(view["access"]["endpoint"])
        if (host.get("host"), host.get("port"), host.get("user")) != (hostname, port, username):
            raise CanaryError("ssh_endpoint_binding_mismatch", "blocked")
        key = host.get("public_key", "")
        parts = key.split()
        if len(parts) != 2 or parts[0] not in {"ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"}:
            raise CanaryError("invalid_host_key", "blocked")
        try:
            if len(base64.b64decode(parts[1], validate=True)) < 32:
                raise ValueError()
        except ValueError:
            raise CanaryError("invalid_host_key", "blocked") from None
        self.record["ssh_identity_verified"] = True
        known_hosts = self.directory / ("known-hosts-" + self.record["run_id"])
        alias = "covenant-canary-" + self.active["job_id"]
        with open(known_hosts, "x", encoding="ascii") as stream:
            stream.write(alias + " " + key + "\n")
        os.chmod(known_hosts, 0o600)
        self.checkpoint("gpu_probe")
        argv = [self.config.get("ssh_binary", "ssh"), "-F", "/dev/null", "-o", "BatchMode=yes",
                "-o", "PasswordAuthentication=no", "-o", "StrictHostKeyChecking=yes",
                "-o", "UserKnownHostsFile=" + str(known_hosts), "-o", "GlobalKnownHostsFile=/dev/null",
                "-o", "HostKeyAlias=" + alias, "-o", "IdentitiesOnly=yes", "-o", "ConnectTimeout=20",
                "-i", self.config["ssh_private_key"], "-p", str(port), username + "@" + hostname, GPU_PROBE]
        try:
            code, output, _ = self.run_process(argv)
        finally:
            known_hosts.unlink(missing_ok=True)
        if code:
            raise CanaryError("gpu_probe_failed")
        devices = list(csv.reader(output.strip().splitlines()))
        if not devices or any(len(row) != 3 or not row[0].strip().startswith("GPU-") or
                              row[1].strip() != self.config["expected_gpu_model"] or not row[2].strip().isdigit() or
                              int(row[2].strip()) < self.config["min_gpu_memory_mib"] for row in devices):
            raise CanaryError("gpu_probe_invalid")
        self.record["gpu_probe_verified"] = True
        self.record["gpu_count"] = len(devices)
        # Only a bounded descriptive GPU model leaves the private evidence store.
        model = devices[0][1].strip()
        self.record["gpu_model"] = model if re.fullmatch(r"[A-Za-z0-9 ._()+/-]{1,80}", model) else None

    def finish_lease(self):
        self.checkpoint("close")
        job_id = self.active["job_id"]
        self.cli("lease", "close", job_id)  # retry-safe, including on recovery runs
        self.active["close_requested_unix"] = time.time()
        self.save_active()
        self.checkpoint("settlement")
        view = self.poll(self.config["settlement_timeout_secs"],
                         lambda: (lambda value: value if value.get("status") in TERMINAL else None)(self.view()))
        charged = integer(view.get("charged_micro_usdc"), "charged_micro_usdc")
        if charged > self.active["lease_ceiling"]:
            raise CanaryError("charge_exceeds_authorized_ceiling")
        verification = self.poll(self.config["settlement_timeout_secs"],
                                 lambda: self.settlement_evidence(view))
        self.record["settlement_verdict"] = verification["verdict"]
        self.record["settlement_verified_onchain"] = verification["verdict"] == "verified_onchain"
        self.record["charged_micro_usdc"] = charged
        self.record["refunded_micro_usdc"] = self.active["lease_ceiling"] - charged
        after, before = self.balance(), self.active["balance_before"]
        if after["deposited_micro_usdc"] != before["deposited_micro_usdc"] or after["withdrawn_micro_usdc"] != before["withdrawn_micro_usdc"]:
            raise CanaryError("dedicated_buyer_balance_changed")
        if after["charged_micro_usdc"] - before["charged_micro_usdc"] != charged or after["available_micro_usdc"] != before["available_micro_usdc"] - charged:
            raise CanaryError("escrow_refund_not_reconciled")
        self.record["balance_reconciled"] = True
        self.active["settlement_verified"] = True
        self.active["charged_micro_usdc"] = charged
        self.save_active()

    def settlement_evidence(self, view):
        value = self.cli("verify", self.active["job_id"])
        if value.get("job_id") != self.active["job_id"]:
            raise CanaryError("verification_job_mismatch")
        if view["status"] == "completed":
            if value.get("verdict") == "verified_onchain":
                return value
            if value.get("verdict") in {"payout_pending", "payout_unconfirmed"}:
                return None
            raise CanaryError("payout_not_verified_onchain")
        if view["charged_micro_usdc"] != 0:
            raise CanaryError("unpaid_job_has_charge")
        rows = self.cli("receipts", "--limit", "100")
        if not isinstance(rows, list):
            raise CanaryError("invalid_receipt_history")
        row = next((row for row in rows if row.get("job_id") == self.active["job_id"]), {})
        if row.get("status") not in TERMINAL or row.get("charged_micro_usdc") != 0 or row.get("receipt_verified") is False:
            raise CanaryError("refund_not_verified")
        if value.get("verdict") not in {"no_payout_due", "no_receipt"}:
            raise CanaryError("unexpected_refund_payout")
        return {"verdict": "refunded_balance_reconciliation"}

    def cleanup(self):
        self.checkpoint("provider_cleanup")
        def check():
            evidence = self.evidence("cleanup")
            instance = evidence.get("provider_instance_id")
            if not isinstance(instance, str) or not instance:
                raise CanaryError("cleanup_identity_missing", "blocked")
            if self.active.get("provider_instance_id") and instance != self.active["provider_instance_id"]:
                raise CanaryError("cleanup_instance_mismatch")
            # Cleanup adapters must authenticate the job->instance mapping even
            # if SSH readiness never arrived, and query the provider itself.
            if evidence.get("binding_verified") is not True or evidence.get("source") != "provider_readback":
                raise CanaryError("cleanup_unverified", "blocked")
            if evidence.get("provider_cost_verified") is True:
                self.budget.observe_provider_cost(self.active, integer(evidence.get("provider_cost_micro_usd"), "provider_cost_micro_usd"))
            if evidence.get("exists") is False:
                return evidence
            return None
        evidence = self.poll(self.config["cleanup_timeout_secs"], check)
        self.record["provider_cleanup_verified"] = True
        self.active["provider_instance_id"] = evidence["provider_instance_id"]
        self.active["cleanup_verified"] = True
        if evidence.get("provider_cost_verified") is True:
            self.active["provider_cost_micro_usd"] = integer(evidence.get("provider_cost_micro_usd"), "provider_cost_micro_usd")
        self.save_active()

    def run(self, preflight_only, recover_only=False):
        self.active = self.budget.active()
        if self.active_path.exists():
            secondary = read_json(self.active_path)
            known = next((entry for entry in self.budget.value["entries"] if entry["job_id"] == secondary.get("job_id")), None)
            if known is None:
                raise CanaryError("orphan_active_record", "blocked")
            if self.active is None:
                self.active_path.unlink()  # resolved atomically before a previous interrupted unlink
        if self.active:
            if self.active.get("coordinator_url") != self.config["coordinator_url"] or self.active.get("buyer_home") != self.config["buyer_home"]:
                raise CanaryError("active_rental_config_mismatch", "blocked")
            if not self.active.get("job_id"):
                raise CanaryError("unresolved_launch_do_not_retry", "blocked")
            self.record["job_id"] = self.active["job_id"]
            self.record["recovery_only"] = True
            if preflight_only:
                raise CanaryError("active_rental_requires_recovery", "blocked")
            # No preflight/capacity dependency may prevent closing an old lease.
            raise CanaryError("recovering_prior_rental")
        if recover_only:
            raise CanaryError("no_unresolved_rental", "blocked")
        if not preflight_only:
            reason = self.budget.pause_reason()
            if reason:
                raise CanaryError(reason, "blocked")
        self.preflight()
        if preflight_only:
            self.record["verdict"] = "preflight_only"
            return
        self.open_lease()
        self.probe(self.wait_ready())

    def reconcile(self):
        errors = []
        if not self.active or not self.active.get("job_id"):
            return errors
        # A failed fresh check must not inherit an earlier run's success flag
        # and erase the rental from the recovery queue.
        self.active.pop("settlement_verified", None)
        self.active.pop("cleanup_verified", None)
        self.save_active()
        for operation in (self.finish_lease, self.cleanup):
            try:
                operation()
            except Exception as error:
                errors.append(error.code if isinstance(error, CanaryError) else type(error).__name__)
        if self.active.get("settlement_verified") and self.active.get("cleanup_verified"):
            self.budget.resolve(self.active, self.active["charged_micro_usdc"], self.active.get("provider_cost_micro_usd"))
            self.active_path.unlink(missing_ok=True)
        if self.budget.value.get("latched_reason"):
            errors.append(self.budget.value["latched_reason"])
        return errors


def endpoint(value):
    # Accept the exact buyer CLI access form without interpreting a remote
    # endpoint as executable shell or accepting server-supplied SSH options.
    try:
        parts = shlex.split(value)
        if not parts or parts[0] != "ssh":
            raise ValueError()
        if len(parts) == 4 and parts[2] == "-p":
            destination, raw_port = parts[1], parts[3]
        elif len(parts) == 4 and parts[1] == "-p":
            destination, raw_port = parts[3], parts[2]
        elif len(parts) == 3 and re.fullmatch(r"-p[0-9]{1,5}", parts[1]):
            destination, raw_port = parts[2], parts[1][2:]
        elif len(parts) == 3 and re.fullmatch(r"-p[0-9]{1,5}", parts[2]):
            destination, raw_port = parts[1], parts[2][2:]
        else:
            raise ValueError()
        user, host = destination.split("@", 1)
        port = int(raw_port)
        if not re.fullmatch(r"[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,63}", user) or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9.-]{0,252}", host) or not 1 <= port <= 65535:
            raise ValueError()
        return host, port, user
    except (ValueError, TypeError):
        raise CanaryError("unsupported_ssh_endpoint", "blocked") from None


def alert_state(previous, verdict, timestamp):
    """Three distinct consecutive six-hour windows, never three rapid retries."""
    window = int(timestamp // WINDOW_SECONDS)
    if verdict == "success":
        return {"window": window, "failure_windows": 0, "alert_due": False}
    if verdict == "preflight_only":
        return previous
    previous_window = previous.get("window")
    count = previous.get("failure_windows", 0)
    if previous_window != window:
        count = count + max(1, window - previous_window) if previous_window is not None else 1
    elif count == 0:
        count = 1
    return {"window": window, "failure_windows": count, "alert_due": count >= 3,
            "last_alert_window": previous.get("last_alert_window")}


PUBLIC_STAGES = {"configuration", "preflight", "funding", "launch", "ready", "ssh_identity", "gpu_probe", "close", "settlement", "provider_cleanup"}
PUBLIC_VERDICTS = {"pending", "success", "failed", "blocked", "recovered"}
PAUSES = {"daily_budget_exhausted", "pilot_budget_exhausted", "provider_budget_breach", "pilot_entry_limit"}


def public_run(record):
    def valid_uuid(value):
        try:
            return value if str(uuid.UUID(value)) == value else None
        except (ValueError, TypeError, AttributeError):
            return None
    run_id = valid_uuid(record.get("run_id"))
    if not run_id:
        raise CanaryError("invalid_run_record", "blocked")
    verdict = record.get("verdict")
    reason = record.get("reason")
    if verdict == "preflight_only":
        verdict, reason = "blocked", "preflight_only"
    reason = reason if isinstance(reason, str) and re.fullmatch(r"[a-z][a-z0-9_]{0,79}", reason) else None
    row = {"run_id": run_id, "job_id": valid_uuid(record.get("job_id")),
           "started_at_unix": integer(record.get("started_at_unix"), "started_at_unix"),
           "finished_at_unix": None if record.get("finished_at_unix") is None else integer(record["finished_at_unix"], "finished_at_unix"),
           "verdict": verdict if verdict in PUBLIC_VERDICTS else "failed",
           "stage": record.get("stage") if record.get("stage") in PUBLIC_STAGES else "configuration",
           "reason": reason, "gpu_model": None}
    model = record.get("gpu_model")
    if isinstance(model, str) and re.fullmatch(r"[A-Za-z0-9 ._()+/-]{1,80}", model):
        row["gpu_model"] = model
    for name in ("charged_micro_usdc", "refunded_micro_usdc"):
        row[name] = None if record.get(name) is None else integer(record[name], name)
    for name in ("ssh_identity_verified", "gpu_probe_verified", "provider_cleanup_verified", "balance_reconciled", "settlement_verified_onchain", "recovery_only"):
        row[name] = record.get(name) is True
    return row


def monitor_state(records, created_at, now, enabled=True, last_success_window=None, last_success_at=None):
    """No provider calls. Absent finished windows count, including host downtime.

    A current pending window has until its boundary to finish. Only a complete
    successful rental resets failures; recovery is not a successful GPU probe.
    Preflight and duplicate-invocation reports cannot suppress missing windows.
    """
    by_window, last_success = {}, last_success_at
    for row in records:
        if row.get("verdict") == "preflight_only" or row.get("reason") in {"another_canary_is_running", "window_already_reserved", "preflight_only"}:
            continue
        if row.get("finished_at_unix") is None:
            continue
        window = row["started_at_unix"] // WINDOW_SECONDS
        if row.get("verdict") == "success":
            by_window[window] = True
            last_success = max(last_success or 0, row["finished_at_unix"])
        else:
            by_window.setdefault(window, False)
    first = created_at // WINDOW_SECONDS
    current = now // WINDOW_SECONDS
    last = current if current in by_window else current - 1
    # Counts can be derived from the latest success; no iteration over downtime.
    successes = [window for window, success in by_window.items() if success and window <= last]
    anchor = max([first - 1] + successes + ([] if last_success_window is None else [last_success_window]))
    count = max(0, last - anchor)
    return {"consecutive_unsuccessful_windows": count, "alert_due": bool(enabled and count >= 3),
            "last_success_at_unix": last_success}


def publish_public_state(state_dir, config, enabled=True, next_due=None):
    """Read durable snapshots and atomically publish an explicit safe whitelist.

    May be invoked every 60s by an independent watchdog, even while run.lock is
    held. public.lock serializes publishers; atomic source files prevent partial
    reads. No subprocess, network access, paid action, or private stdout occurs.
    """
    directory = Path(state_dir)
    with open(directory / "public.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        now = int(time.time())
        journal = read_json(directory / "budget.json")
        entries = journal["entries"]
        identity = journal["identity"]
        totals = {"daily_limit_micro_usd": identity["daily_limit_micro_usd"], "pilot_limit_micro_usd": identity["pilot_limit_micro_usd"],
                  "daily_reserved_micro_usd": sum(e["counted_micro_usd"] for e in entries if e.get("resolved_at_unix") is None or e["resolved_at_unix"] > now - DAY_SECONDS),
                  "pilot_reserved_micro_usd": sum(e["counted_micro_usd"] for e in entries)}
        pause = journal.get("latched_reason")
        if not pause and any(e.get("resolved_at_unix") is None for e in entries):
            pause = "unresolved_rental"
        if not pause and totals["pilot_reserved_micro_usd"] + config["operating_ceiling"] > totals["pilot_limit_micro_usd"]:
            pause = "pilot_budget_exhausted"
        if not pause and totals["daily_reserved_micro_usd"] + config["operating_ceiling"] > totals["daily_limit_micro_usd"]:
            pause = "daily_budget_exhausted"
        enabled = bool(enabled and pause not in PAUSES)
        records = [read_json(path) for path in (directory / "runs").glob("*.json")]
        records.sort(key=lambda r: (r["started_at_unix"], r.get("finished_at_unix") or now, r["run_id"]), reverse=True)
        rows = [public_run(row) for row in records[:HISTORY_LIMIT]]
        monitor = monitor_state(records, journal["created_at_unix"], now, enabled,
                                journal.get("last_success_window"), journal.get("last_success_at_unix"))
        state = {"schema": 1, "generated_at_unix": now, "enabled": enabled, "cadence_seconds": WINDOW_SECONDS,
                 "next_due_at_unix": (next_due if next_due is not None else (now // WINDOW_SECONDS + 1) * WINDOW_SECONDS) if enabled else None,
                 "budget": totals, "latest": rows[0] if rows else None, "history": rows, "monitor": monitor, "pause_reason": pause}
        target = Path(config["public_state_path"])
        # Deployment creates the separate 0755 parent; never chmod the private directory.
        if not target.parent.is_dir() or target.parent.resolve() == directory.resolve():
            raise CanaryError("invalid_public_state_directory", "blocked")
        atomic_json(target, state, mode=0o644)
        return state


def interrupted(signum, _frame):
    raise CanaryError("interrupted_" + str(signum))


def monitor_once(directory, config, enabled=True, next_due=None):
    """Independent no-spend watchdog. Run every minute, separate from the rental."""
    with open(directory / "monitor.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state = publish_public_state(directory, config, enabled, next_due)
        path = directory / "alert-state.json"
        previous = read_json(path) if path.exists() else {}
        due = state["monitor"]["alert_due"]
        window = state["generated_at_unix"] // WINDOW_SECONDS
        recovery = previous.get("incident_open") is True and not due and state["monitor"]["consecutive_unsuccessful_windows"] == 0
        if config.get("alert_command") and ((due and previous.get("last_alert_window") != window) or recovery):
            payload = {"schema": 1, "type": "rental_canary_recovered" if recovery else "rental_canary_missing_or_unsuccessful", "state": state}
            try:
                result = subprocess.run(config["alert_command"], input=json.dumps(payload), text=True,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                        timeout=config["command_timeout_secs"], check=False)
                if result.returncode == 0:
                    previous.update({"last_alert_window": window, "incident_open": due})
            except (OSError, subprocess.TimeoutExpired):
                pass  # Retry while due; public evidence never pretends alert delivery.
        atomic_json(path, previous)
        return state


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--preflight", action="store_true", help="read-only; never a passing rental verdict")
    parser.add_argument("--once", action="store_true", help="one run or recovery; never a background scheduler")
    parser.add_argument("--recover-only", action="store_true", help="reconcile an existing rental; never authorize a new one")
    parser.add_argument("--monitor", action="store_true", help="publish heartbeat and check missing windows; never rent")
    parser.add_argument("--disabled", action="store_true", help="with --monitor, publish intentionally disabled scheduling")
    args = parser.parse_args(argv)
    os.umask(0o077)
    directory = Path(args.state_dir)
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "runs").mkdir(exist_ok=True)
    if args.monitor:
        if args.preflight or args.once or args.recover_only:
            parser.error("--monitor cannot be combined with --once or --preflight")
        try:
            state = monitor_once(directory, load_config(args.config), enabled=not args.disabled)
            print(json.dumps(state, sort_keys=True))
            return 0
        except Exception:
            print(json.dumps({"schema": 1, "error": "monitor_unavailable"}))
            return 2
    if args.disabled:
        parser.error("--disabled requires --monitor")
    if args.recover_only and (args.once or args.preflight):
        parser.error("--recover-only cannot be combined with --once or --preflight")
    record = {"version": 1, "run_id": str(uuid.uuid4()), "started_at_unix": int(time.time()),
              "verdict": "pending", "stage": "configuration"}
    runner, lock, owns_lock = None, None, False
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, interrupted)
    atomic_json(directory / "runs" / (record["run_id"] + ".json"), record)
    try:
        lock = open(directory / "run.lock", "a")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            owns_lock = True
        except BlockingIOError:
            raise CanaryError("another_canary_is_running", "blocked") from None
        runner = Canary(directory, load_config(args.config), record)
        runner.run(args.preflight, args.recover_only)
    except (Exception, KeyboardInterrupt) as error:
        record["verdict"] = error.verdict if isinstance(error, CanaryError) else "failed"
        record["reason"] = error.code if isinstance(error, CanaryError) else type(error).__name__
    finally:
        # A second signal must not interrupt bounded cleanup and erase the
        # financial result; SIGKILL retains active.json for the next run.
        for signum in (signal.SIGTERM, signal.SIGINT):
            signal.signal(signum, signal.SIG_IGN)
        if runner and not args.preflight:
            try:
                errors = runner.reconcile()
            except Exception as error:
                errors = [error.code if isinstance(error, CanaryError) else type(error).__name__]
            if errors:
                record["reconciliation_errors"] = errors
                record["verdict"] = "failed"
                record.setdefault("reason", errors[0])
            elif record.get("gpu_probe_verified") and record.get("provider_cleanup_verified") and record.get("balance_reconciled") and record.get("settlement_verdict") == "verified_onchain" and not record.get("reason"):
                record["verdict"] = "success"
            elif record.get("recovery_only") and not errors:
                record["verdict"] = "recovered"
                record["reason"] = "prior_rental_reconciled"
        if record["verdict"] == "pending":
            record["verdict"] = "failed"
        record["finished_at_unix"] = int(time.time())
        if runner and record["verdict"] == "success":
            runner.budget.record_success(record)
        atomic_json(directory / "runs" / (record["run_id"] + ".json"), record)
        if owns_lock:
            atomic_json(directory / "latest.json", record)
            if runner:
                try:
                    monitor_once(directory, runner.config)
                except Exception:
                    record["public_monitor_error"] = True
            # Bound verdict retention; financial reservations are never pruned.
            paths = sorted((directory / "runs").glob("*.json"), key=lambda p: p.stat().st_mtime_ns, reverse=True)
            for path in paths[PRIVATE_RUN_LIMIT:]:
                path.unlink(missing_ok=True)
        atomic_json(directory / "runs" / (record["run_id"] + ".json"), record)
        if lock:
            lock.close()
    print(json.dumps(record, sort_keys=True))
    return 0 if record["verdict"] in {"success", "recovered"} else 2 if record["verdict"] in {"blocked", "preflight_only"} else 1


if __name__ == "__main__":
    sys.exit(main())
