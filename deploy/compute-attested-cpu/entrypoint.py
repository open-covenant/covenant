#!/usr/bin/env python3
"""Separate SSH and collector roles; no provider credentials enter either."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

import collector
from memory_guard import RuntimeGuard


def run_ssh():
    collector.require(os.geteuid() == 0, "SSH privilege separation requires container root")
    collector.require("COVENANT_COLLECTOR_TOKEN" not in os.environ,
                      "collector token must not enter the SSH container")
    policy = collector.environment_policy()
    guard = RuntimeGuard()
    # Before key generation or any workspace/session access. Unsupported
    # kernels, mounts or cgroup layouts stop this role; there is no fallback.
    guard.check("ssh")
    guard.invalidate()
    deadline = time.monotonic() + (
        policy["lease_expires_at_ms"] - int(time.time() * 1000)
    ) / 1000
    os.umask(0o077)
    runtime = Path("/run/covenant")
    runtime.mkdir(mode=0o700)
    Path("/run/sshd").mkdir(mode=0o755, exist_ok=True)
    key = runtime / "host_key"
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "",
                    "-f", str(key)], check=True)
    public = subprocess.check_output(["ssh-keygen", "-y", "-f", str(key)]).decode().strip()
    collector.ssh_key(public)
    public_dir = Path("/host-public")
    public_dir.chmod(0o755)
    temporary = public_dir / "host_key.pub.new"
    temporary.write_text(public + "\n")
    temporary.chmod(0o644)
    os.replace(temporary, public_dir / "host_key.pub")
    authorized = Path("/run/authorized_keys")
    authorized.write_text("restrict " + policy["buyer_ssh_public_key"] + "\n")
    authorized.chmod(0o644)
    # OpenSSH reads authorized_keys as the target uid. It is public and readable,
    # while its root-owned parent prevents modification by buyer sessions.
    config = runtime / "sshd_config"
    config.write_text("""Port 1022
ListenAddress 0.0.0.0
HostKey /run/covenant/host_key
PidFile /run/covenant/sshd.pid
AuthorizedKeysFile /run/authorized_keys
AllowUsers covenant
PermitRootLogin no
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitEmptyPasswords no
UsePAM no
StrictModes yes
PubkeyAuthentication yes
PubkeyAcceptedAlgorithms ssh-ed25519
HostKeyAlgorithms ssh-ed25519
DisableForwarding yes
PermitTTY no
PermitTunnel no
PermitUserEnvironment no
PermitUserRC no
X11Forwarding no
MaxAuthTries 3
MaxSessions 2
MaxStartups 2:50:4
LoginGraceTime 20
LogLevel ERROR
Subsystem sftp internal-sftp
""")
    subprocess.run(["/usr/sbin/sshd", "-t", "-f", str(config)], check=True)
    guard.publish(policy, public)
    # The master retains only standard container privilege-separation rights.
    # PID 1 exits at lease expiry, terminating every session in this container.
    child = subprocess.Popen(["/usr/sbin/sshd", "-D", "-e", "-f", str(config)],
                             env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"})
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        while not stopping and child.poll() is None and (
            int(time.time() * 1000) < policy["lease_expires_at_ms"]
        ) and time.monotonic() < deadline:
            # The immutable entrypoint keeps enforcing the actual runtime, not
            # just its intended Compose configuration. Any failed read/check
            # invalidates collection and terminates the SSH container.
            guard.publish(policy, public)
            time.sleep(0.25)
    finally:
        try:
            guard.invalidate()
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()


if __name__ == "__main__":
    if sys.argv[1:] == ["ssh"]:
        run_ssh()
    elif sys.argv[1:] == ["collector"]:
        collector.require(os.geteuid() == 10002, "collector must run as uid10002")
        collector.main()
    elif sys.argv[1:] == ["outbound"]:
        collector.require(os.geteuid() == 10002, "outbound collector must run as uid10002")
        import ephemeral_collector
        ephemeral_collector.main()
    else:
        raise SystemExit("expected ssh, collector or outbound role")
