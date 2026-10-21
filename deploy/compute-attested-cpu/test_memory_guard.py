import json
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import memory_guard as m


def mount_line(number, path, kind, super_options="rw", options="rw", root="/"):
    return f"{number} 1 0:{number} {root} {path} {options} - {kind} {kind} {super_options}\n"


def valid_mounts(role):
    text = mount_line(1, "/", "overlay")
    text += mount_line(2, "/proc", "proc")
    text += mount_line(3, "/sys/fs/cgroup", "cgroup2", options="ro")
    text += mount_line(4, "/host-public", "tmpfs", "rw,noswap,size=1024k",
                       options=("ro" if role == "collector" else "rw") +
                       ",nosuid,nodev,noexec")
    if role == "ssh":
        for number, path, size, extra in [
            (5, "/run", "8192k", ",noexec"), (6, "/tmp", "16384k", ",noexec"),
            (7, "/workspace", "131072k", ""),
        ]:
            text += mount_line(number, path, "tmpfs", f"rw,noswap,size={size}",
                               options="rw,nosuid,nodev" + extra)
    return text


class MemoryGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.proc = self.directory / "proc"
        self.group = self.directory / "cgroup"
        self.public = self.directory / "public"
        (self.proc / "self").mkdir(parents=True)
        (self.proc / "sys/kernel").mkdir(parents=True)
        self.group.mkdir()
        self.public.mkdir()
        self.now = 123456
        self.guard = m.RuntimeGuard(self.proc, self.group, self.public,
                                   clock=lambda: self.now, core_limits=lambda: (0, 0))
        self.configure("ssh")

    def configure(self, role):
        (self.proc / "swaps").write_text("Filename\tType\tSize\tUsed\tPriority\n")
        (self.proc / "self/cgroup").write_text("0::/\n")
        (self.proc / "self/mountinfo").write_text(valid_mounts(role))
        (self.proc / "sys/kernel/core_pattern").write_text("core\n")
        limit = m.SSH_MEMORY_BYTES if role == "ssh" else m.COLLECTOR_MEMORY_BYTES
        (self.group / "memory.max").write_text(f"{limit}\n")
        for name in ("memory.swap.max", "memory.swap.current"):
            (self.group / name).write_text("0\n")

    def test_complete_kernel_profile_accepted_for_both_roles(self):
        for role in ("ssh", "collector"):
            self.configure(role)
            self.guard.check(role)

    def test_swap_enabled_missing_unknown_or_used_rejected(self):
        cases = [
            (self.proc / "swaps", "Filename Type Size Used Priority\n/dev/sda partition 1 0 -2\n"),
            (self.proc / "swaps", ""),
            (self.group / "memory.swap.max", "max\n"),
            (self.group / "memory.swap.max", "1\n"),
            (self.group / "memory.swap.current", "1\n"),
            (self.group / "memory.max", "max\n"),
            (self.proc / "self/cgroup", "0::/host/container\n"),
            (self.proc / "sys/kernel/core_pattern", "|/usr/lib/systemd/systemd-coredump %P\n"),
            (self.proc / "sys/kernel/core_pattern", ""),
        ]
        for path, value in cases:
            with self.subTest(path=path.name, value=value):
                self.configure("ssh")
                path.write_text(value)
                with self.assertRaises(m.UnsafeRuntime):
                    self.guard.check("ssh")
        self.configure("ssh")
        (self.group / "memory.swap.max").unlink()
        with self.assertRaises(OSError):
            self.guard.check("ssh")
        self.configure("ssh")
        self.guard.core_limits = lambda: (0, -1)
        with self.assertRaises(m.UnsafeRuntime):
            self.guard.check("ssh")

    def test_mount_type_flags_limits_and_metadata_substitution_rejected(self):
        text = valid_mounts("ssh")
        cases = [
            text.replace("noswap", "swap"),
            text.replace("size=131072k", "size=262144k"),
            text.replace("size=8192k", "size=0"),
            text.replace("/workspace rw,nosuid,nodev - tmpfs", "/workspace rw - ext4"),
            text.replace("/run rw,nosuid,nodev,noexec", "/run rw,nosuid,nodev"),
            text.replace("/sys/fs/cgroup ro - cgroup2", "/sys/fs/cgroup ro - cgroup"),
            text + mount_line(8, "/workspace/data", "ext4"),
            text + mount_line(8, "/proc/swaps", "tmpfs"),
            text + mount_line(8, "/sys/fs/cgroup/memory.swap.max", "tmpfs"),
            text + mount_line(8, "/host-public", "tmpfs"),
            text.replace("0:4 / /host-public", "0:4 /subdir /host-public"),
        ]
        for value in cases:
            with self.subTest(value=value):
                (self.proc / "self/mountinfo").write_text(value)
                with self.assertRaises(m.UnsafeRuntime):
                    self.guard.check("ssh")

    def status_metadata(self, **changes):
        values = dict(st_mode=stat.S_IFREG | 0o644, st_uid=0, st_gid=0,
                      st_nlink=1, st_size=500)
        values.update(changes)
        return SimpleNamespace(**values)

    def peer_check(self, policy, key, **metadata):
        with mock.patch.object(m.os, "fstat", return_value=self.status_metadata(**metadata)):
            self.guard.check_peer(policy, key)

    def publish_fixture(self):
        policy = {"job_id": "lease-one", "lease_generation": "cvm-one", "lease_expires_at_ms": 200000}
        key = "ssh-ed25519 public-one"
        self.configure("ssh")
        self.guard.publish(policy, key)
        self.configure("collector")
        return policy, key

    def test_status_binds_full_policy_key_and_short_monotonic_window(self):
        policy, key = self.publish_fixture()
        self.peer_check(policy, key)
        self.now += m.MAX_STATUS_AGE_MS
        self.peer_check(policy, key)
        self.now += 1
        with self.assertRaises(m.UnsafeRuntime):
            self.peer_check(policy, key)
        self.now -= m.MAX_STATUS_AGE_MS + 2
        with self.assertRaises(m.UnsafeRuntime):
            self.peer_check(policy, key)
        self.now += 1
        with self.assertRaises(m.UnsafeRuntime):
            self.peer_check(dict(policy, lease_generation="replacement"), key)
        with self.assertRaises(m.UnsafeRuntime):
            self.peer_check(policy, "ssh-ed25519 public-two")

    def test_status_requires_root_regular_strict_file_and_schema(self):
        policy, key = self.publish_fixture()
        for mutation in (
            {"st_uid": 10001}, {"st_gid": 10001}, {"st_nlink": 2},
            {"st_mode": stat.S_IFREG | 0o666}, {"st_mode": stat.S_IFIFO | 0o644},
            {"st_size": 2049},
        ):
            with self.subTest(mutation=mutation), self.assertRaises(m.UnsafeRuntime):
                self.peer_check(policy, key, **mutation)
        status_path = self.public / m.STATUS_NAME
        original = status_path.read_text()
        parsed = json.loads(original)
        for field, value in (
            ("checked_at_boottime_ms", True), ("checked_at_boottime_ms", 1.0),
            ("schema_version", True), ("unexpected", "field"),
        ):
            modified = dict(parsed)
            modified[field] = value
            status_path.write_text(json.dumps(modified))
            with self.subTest(field=field, value=value), self.assertRaises(m.UnsafeRuntime):
                self.peer_check(policy, key)
        status_path.write_text(original[:-1] + ',"schema_version":1}')
        with self.assertRaises(m.UnsafeRuntime):
            self.peer_check(policy, key)
        status_path.unlink()
        target = self.public / "elsewhere"
        target.write_text(original)
        status_path.symlink_to(target)
        with self.assertRaises(OSError):
            self.peer_check(policy, key)

    def test_unsafe_watchdog_update_cannot_refresh_status(self):
        policy, key = self.publish_fixture()
        self.configure("ssh")
        previous = (self.public / m.STATUS_NAME).read_bytes()
        self.now += 500
        (self.group / "memory.swap.max").write_text("1024\n")
        with self.assertRaises(m.UnsafeRuntime):
            self.guard.publish(policy, key)
        self.assertEqual((self.public / m.STATUS_NAME).read_bytes(), previous)
        self.guard.invalidate()
        self.assertFalse((self.public / m.STATUS_NAME).exists())


if __name__ == "__main__":
    unittest.main()
