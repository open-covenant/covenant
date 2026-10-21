"""Fail-closed Linux runtime checks, not a substitute for guest/KMS trust.

Only the measured entrypoint calls the status writer. The collector consumes
its short-lived status through a read-only, non-swappable shared tmpfs mount.
No environment variable or command-line flag disables these checks.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import stat
import time


MIB = 1024 * 1024
MAX_STATUS_AGE_MS = 2000
STATUS_NAME = "memory-status.json"
PROFILE = "covenant-memory-v1"
SSH_MEMORY_BYTES = 768 * MIB
COLLECTOR_MEMORY_BYTES = 256 * MIB
WORKSPACE_BYTES = 128 * MIB
PUBLIC_BYTES = MIB


class UnsafeRuntime(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise UnsafeRuntime(message)


def boottime_ms():
    # CLOCK_BOOTTIME is shared by the two ordinary container time namespaces,
    # includes suspend, and cannot be extended by a wall-clock rollback.
    return time.clock_gettime_ns(time.CLOCK_BOOTTIME) // 1_000_000


def bounded_read(path, maximum):
    with Path(path).open("rb") as source:
        value = source.read(maximum + 1)
    require(len(value) <= maximum, "oversized runtime metadata")
    return value.decode("ascii")


def mount_path(value):
    # mountinfo escapes whitespace and backslashes using these four sequences.
    decoded = re.sub(r"\\(040|011|012|134)",
                     lambda match: chr(int(match.group(1), 8)), value)
    require("\\" not in decoded and "\0" not in decoded, "unsupported mount path")
    return decoded


def mounts_from(raw):
    mounts = {}
    for line in raw.splitlines():
        parts = line.split()
        require(len(parts) >= 10 and parts.count("-") == 1, "invalid mount metadata")
        separator = parts.index("-")
        require(separator >= 6 and len(parts) == separator + 4, "invalid mount metadata")
        point = mount_path(parts[4])
        require(point.startswith("/") and point not in mounts, "ambiguous mount")
        mounts[point] = {
            "root": mount_path(parts[3]), "options": set(parts[5].split(",")),
            "type": parts[separator + 1],
            "super_options": set(parts[separator + 3].split(",")),
        }
    require(mounts, "missing mount metadata")
    return mounts


def size_bytes(options):
    values = [item[5:] for item in options if item.startswith("size=")]
    require(len(values) == 1, "missing tmpfs size")
    match = re.fullmatch(r"([1-9][0-9]*)([kmg]?)", values[0])
    require(match is not None, "invalid tmpfs size")
    return int(match.group(1)) * {"": 1, "k": 1024, "m": MIB, "g": 1024 * MIB}[match.group(2)]


def require_tmpfs(mounts, path, size, *, readonly=False, noexec=True):
    mount = mounts.get(path)
    require(mount is not None and mount["type"] == "tmpfs" and mount["root"] == "/",
            "required whole tmpfs mount unavailable")
    options, super_options = mount["options"], mount["super_options"]
    require({"nosuid", "nodev"} <= options, "unsafe tmpfs mount flags")
    require(not noexec or "noexec" in options, "tmpfs execution is enabled")
    require(("ro" if readonly else "rw") in options, "unexpected tmpfs access mode")
    require("noswap" in super_options, "tmpfs swap is enabled or unknown")
    require(size_bytes(super_options) == size, "unexpected tmpfs size")
    require(not any(point.startswith(path + "/") for point in mounts),
            "unexpected nested storage mount")


class RuntimeGuard:
    def __init__(self, proc="/proc", cgroup="/sys/fs/cgroup", public="/host-public",
                 clock=boottime_ms, core_limits=None):
        # Alternate paths/clocks are constructor-only seams for offline tests;
        # the executable never accepts them from provider or request inputs.
        self.proc, self.cgroup, self.public = Path(proc), Path(cgroup), Path(public)
        self.clock = clock
        self.core_limits = core_limits or (lambda: resource.getrlimit(resource.RLIMIT_CORE))

    def check(self, role):
        require(role in {"ssh", "collector"}, "invalid runtime role")
        mounts = mounts_from(bounded_read(self.proc / "self/mountinfo", 512 * 1024))
        proc = mounts.get("/proc")
        require(proc is not None and proc["type"] == "proc" and proc["root"] == "/",
                "kernel procfs unavailable")
        # Reject bind-overridden proc evidence, even if its bytes look safe.
        require(not any(path in mounts for path in
                        ("/proc/swaps", "/proc/self", "/proc/self/cgroup",
                         "/proc/self/mountinfo")), "overridden kernel metadata")
        swaps = bounded_read(self.proc / "swaps", 65536).splitlines()
        require(len(swaps) == 1 and swaps[0].split() ==
                ["Filename", "Type", "Size", "Used", "Priority"],
                "guest swap is enabled or unknown")
        group = mounts.get("/sys/fs/cgroup")
        require(group is not None and group["type"] == "cgroup2" and group["root"] == "/",
                "private cgroup v2 unavailable")
        require(not any(path.startswith("/sys/fs/cgroup/") for path in mounts),
                "overridden cgroup metadata")
        require(bounded_read(self.proc / "self/cgroup", 4096) == "0::/\n",
                "private unified cgroup required")
        expected = SSH_MEMORY_BYTES if role == "ssh" else COLLECTOR_MEMORY_BYTES
        require(bounded_read(self.cgroup / "memory.max", 64).strip() == str(expected),
                "unexpected container memory limit")
        require(bounded_read(self.cgroup / "memory.swap.max", 64).strip() == "0",
                "container swap is enabled or unknown")
        require(bounded_read(self.cgroup / "memory.swap.current", 64).strip() == "0",
                "container has swapped pages")
        require(self.core_limits() == (0, 0), "core dumps must be disabled")
        # Linux pipe handlers can ignore RLIMIT_CORE. Do not accept that path,
        # even with a zero resource limit; its policy belongs to the guest.
        pattern = bounded_read(self.proc / "sys/kernel/core_pattern", 4096).strip()
        require(pattern and not pattern.startswith("|") and "\n" not in pattern,
                "piped or unknown core dump handler")
        require_tmpfs(mounts, "/host-public", PUBLIC_BYTES, readonly=(role == "collector"))
        if role == "ssh":
            require_tmpfs(mounts, "/run", 8 * MIB)
            require_tmpfs(mounts, "/tmp", 16 * MIB)
            require_tmpfs(mounts, "/workspace", WORKSPACE_BYTES, noexec=False)

    @staticmethod
    def bindings(policy, public_key):
        encoded = json.dumps(policy, sort_keys=True, separators=(",", ":"),
                             ensure_ascii=False).encode()
        return {
            "schema_version": 1,
            "profile": PROFILE,
            "policy_sha256": hashlib.sha256(encoded).hexdigest(),
            "host_key_sha256": hashlib.sha256(public_key.strip().encode("ascii")).hexdigest(),
        }

    def publish(self, policy, public_key):
        self.check("ssh")
        value = self.bindings(policy, public_key)
        value["checked_at_boottime_ms"] = self.clock()
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
        temporary = self.public / (STATUS_NAME + ".new")
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                             os.O_NOFOLLOW | os.O_CLOEXEC, 0o644)
        try:
            os.fchmod(descriptor, 0o644)
            with os.fdopen(descriptor, "wb") as output:
                descriptor = None
                output.write(raw)
                output.flush()
            os.replace(temporary, self.public / STATUS_NAME)
        finally:
            if descriptor is not None:
                os.close(descriptor)
            temporary.unlink(missing_ok=True)

    def invalidate(self):
        (self.public / STATUS_NAME).unlink(missing_ok=True)

    def check_peer(self, policy, public_key):
        self.check("collector")
        descriptor = os.open(self.public / STATUS_NAME, os.O_RDONLY |
                             os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            metadata = os.fstat(descriptor)
            require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0
                    and metadata.st_gid == 0 and stat.S_IMODE(metadata.st_mode) == 0o644
                    and metadata.st_nlink == 1 and metadata.st_size <= 2048,
                    "unsafe SSH runtime status file")
            with os.fdopen(descriptor, "rb") as source:
                descriptor = None
                raw = source.read(2049)
            require(len(raw) <= 2048, "oversized SSH runtime status")

            def pairs(items):
                result = {}
                for name, value in items:
                    require(name not in result, "duplicate runtime status field")
                    result[name] = value
                return result
            value = json.loads(raw, object_pairs_hook=pairs)
            expected = self.bindings(policy, public_key)
            require(type(value) is dict and set(value) ==
                    set(expected) | {"checked_at_boottime_ms"}, "invalid SSH runtime status")
            checked = value.pop("checked_at_boottime_ms")
            require(type(checked) is int and 0 <= self.clock() - checked <= MAX_STATUS_AGE_MS,
                    "stale SSH runtime status")
            require(value == expected and all(type(value[key]) is type(expected[key])
                                             for key in expected),
                    "SSH runtime status belongs to another key or lease")
        finally:
            if descriptor is not None:
                os.close(descriptor)
