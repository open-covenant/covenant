#!/usr/bin/env python3
"""Local Docker + real OpenSSH smoke. Fake guest evidence is never TEE acceptance."""
import argparse
import base64
import copy
import http.server
import json
import os
from pathlib import Path
import secrets
import select
import shlex
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

import collector as c
import memory_guard as memory
from test_collector import EVENTS, INFO, POLICY


def outbound_ssh_smoke(directory, ssh_port, identity, policy):
    """Real SSH over relay; only the hardware evidence provider is a test double."""
    from outbound_relay import RelayState, serve_relay
    from outbound_worker import OutboundWorker
    from relay_client import RelayClient
    from transport_protocol import HttpTransport

    session_id, buyer_token = str(uuid.uuid4()), secrets.token_hex(32)
    expiry = min(policy["lease_expires_at_ms"], int(time.time() * 1000) + 90_000)
    relay = serve_relay(RelayState(session_id, expiry, buyer_token))
    endpoint = f"http://127.0.0.1:{relay.server_port}"
    relay_thread = threading.Thread(target=relay.serve_forever, daemon=True)
    relay_thread.start()
    errors = []

    class SmokeEvidence:
        def identity(self):
            result = copy.deepcopy(identity)
            result.update(host="relay.example.com", port=443,
                          transport={"kind": "outbound_relay"})
            return result

        def collect(self, challenge):
            return {"schema_version": 1, "verification_status": "unverified",
                    "challenge": copy.deepcopy(challenge), "identity": self.identity(),
                    "dstack_quote": {"fake_for_transport_smoke_only": True}}

        def check_stream(self, expected):
            assert self.identity() == expected

    def connector(address, timeout):
        assert address == ("ssh", 1022)
        return socket.create_connection(("127.0.0.1", ssh_port), timeout=timeout)

    worker = OutboundWorker("https://relay.example.com", session_id, SmokeEvidence(), expiry,
                            transport=HttpTransport(endpoint, session_id, allow_loopback=True),
                            ssh_connector=connector)

    def worker_main():
        try:
            worker.run()
        except Exception as error:
            errors.append(type(error).__name__)

    worker_thread = threading.Thread(target=worker_main, daemon=True)
    worker_thread.start()
    try:
        client = RelayClient(endpoint, session_id, buyer_token)
        observed = client.identity()
        assert observed["guest_channel_key"] == identity["guest_channel_key"]
        challenge = {"challenge_id": str(uuid.uuid4()),
                     "expires_at_ms": int(time.time() * 1000) + 60_000}
        quoted = client.quote(challenge)
        assert quoted["dstack_quote"] == {"fake_for_transport_smoke_only": True}
        client.pin_instance(observed["guest_info"]["instance_id"])
        token_file = directory / "relay-buyer-token"
        token_file.write_text(buyer_token)
        token_file.chmod(0o600)
        relay_known_hosts = directory / "relay-known-hosts"
        key = observed["guest_channel_key"]
        relay_known_hosts.write_text(
            f'[relay.example.com]:443 {key["algorithm"]} {key["key_base64"]}\n')
        proxy = shlex.join([sys.executable, str(Path(__file__).with_name("relay_client.py")),
                            "--endpoint", endpoint, "--session-id", session_id,
                            "--buyer-token-file", str(token_file),
                            "--verified-challenge-id", challenge["challenge_id"]])
        options = ["-F", "/dev/null", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
                   "-o", "IdentityAgent=none", "-o", "StrictHostKeyChecking=yes",
                   "-o", f"UserKnownHostsFile={relay_known_hosts}", "-o", "GlobalKnownHostsFile=/dev/null",
                   "-o", "HostKeyAlgorithms=ssh-ed25519", "-o", "VerifyHostKeyDNS=no",
                   "-o", "UpdateHostKeys=no", "-o", "CheckHostIP=no", "-o", "ClearAllForwardings=yes",
                   "-o", "ConnectionAttempts=1", "-o", "ConnectTimeout=15", "-o", "LogLevel=ERROR",
                   "-o", "ProxyCommand=" + proxy]
        payload = "outbound-real-ssh-synthetic\n" * 2048
        command = "python3 -c " + shlex.quote(
            "import os,sys; assert os.getuid()==10001; data=sys.stdin.read(); "
            "sys.stdout.write('relay-ssh-ok\\n'+data)")
        result = run("ssh", *options, "-i", str(directory / "buyer"), "-p", "443",
                     "covenant@relay.example.com", command, input_text=payload)
        assert result.stdout == "relay-ssh-ok\n" + payload
        client.close_stream()
        worker_thread.join(timeout=10)
        assert not worker_thread.is_alive(), "relay worker did not terminate closed SSH stream"
        assert not errors, f"relay worker failed: {errors}"
        print("PASS: real strict-host-key SSH through outbound relay; hardware evidence mocked.")
    finally:
        relay.shutdown()
        relay.server_close()
        relay_thread.join(timeout=2)


def run(*arguments, check=True, env=None, input_text=None):
    result = subprocess.run(arguments, text=True, capture_output=True, env=env,
                            input=input_text, timeout=60)
    if check and result.returncode:
        # Do not print command arguments: environment inputs can be confidential.
        raise RuntimeError(f"{arguments[0]} failed: {result.stderr[-2000:]}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    args = parser.parse_args()
    prefix = "covenant-attested-smoke-" + uuid.uuid4().hex[:12]
    containers, volumes = [], []
    guest_calls = []
    token = secrets.token_hex(32)
    now = int(time.time() * 1000)
    directory_context = tempfile.TemporaryDirectory(prefix="cc-smoke-", dir="/tmp")
    directory = Path(directory_context.name)
    guest_server = None
    try:
        for name in ("buyer", "wrong-buyer"):
            run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "",
                "-f", str(directory / name))
        buyer_public = " ".join((directory / "buyer.pub").read_text().split()[:2])
        policy = copy.deepcopy(POLICY)
        policy.update(buyer_ssh_public_key=buyer_public, lease_expires_at_ms=now + 300_000)
        encoded = base64.b64encode(c.canonical(policy)).decode()
        process_env = dict(os.environ, COVENANT_COLLECTOR_TOKEN=token,
                           COVENANT_LEASE_POLICY_B64=encoded)

        class GuestHandler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, value):
                raw = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                if self.path != "/Info":
                    self.send_error(404)
                    return
                self.reply(INFO)

            def do_POST(self):
                body = c.strict_json(self.rfile.read(int(self.headers["Content-Length"])))
                if self.path != "/GetQuote":
                    self.send_error(404)
                    return
                guest_calls.append(body)
                self.reply({"quote": "abcd", "report_data": body["report_data"],
                            "event_log": EVENTS, "vm_config": INFO["vm_config"]})

        guest_socket = str(directory / "guest.sock")
        guest_server = socketserver.UnixStreamServer(guest_socket, GuestHandler)
        os.chmod(guest_socket, 0o666)
        guest_thread = threading.Thread(target=guest_server.serve_forever, daemon=True)
        guest_thread.start()
        def create_volume(suffix, tmpfs_options=None):
            name = prefix + "-" + suffix
            options = (["--driver", "local", "--opt", "type=tmpfs", "--opt", "device=tmpfs",
                        "--opt", "o=" + tmpfs_options]
                       if tmpfs_options is not None else [])
            run("docker", "volume", "create", *options, name)
            volumes.append(name)
            return name

        public_options = "rw,nosuid,nodev,noexec,noswap,size=1m,mode=755"

        def ssh_memory_mounts(suffix, workspace_noswap=True):
            specifications = {
                "run": "rw,nosuid,nodev,noexec,noswap,size=8m,mode=755",
                "tmp": "rw,nosuid,nodev,noexec,noswap,size=16m,mode=1777",
                "workspace": "rw,nosuid,nodev,noswap,size=128m,mode=700,uid=10001,gid=10001",
            }
            if not workspace_noswap:
                specifications["workspace"] = specifications["workspace"].replace(",noswap", "")
            mounts = []
            for path, options in specifications.items():
                volume = create_volume(suffix + "-" + path, options)
                mounts += ["--mount", f"type=volume,src={volume},dst=/{path},volume-nocopy"]
            return mounts

        public_volume = create_volume("public", public_options)
        state_volume = create_volume("state")
        main_memory = ssh_memory_mounts("main")
        common = [
            "--read-only", "--restart", "no", "--cap-drop", "ALL",
            "--security-opt", "no-new-privileges:true",
            "--env", "COVENANT_LEASE_POLICY_B64",
            "--ulimit", "core=0:0",
            "--cgroupns", "private",
        ]
        ssh_runtime = [
            "--user", "0:0", "--cap-add", "CHOWN", "--cap-add", "SETUID",
            "--cap-add", "SETGID", "--cap-add", "SYS_CHROOT",
            "--pids-limit", "128", "--memory", "768m", "--memory-swap", "768m",
        ]
        collector_runtime = [
            "--user", "10002:10002", "--env", "COVENANT_COLLECTOR_TOKEN",
            "--mount", f"type=bind,src={guest_socket},dst=/var/run/dstack.sock,readonly",
            "--pids-limit", "16", "--memory", "256m", "--memory-swap", "256m",
        ]
        ssh_name = prefix + "-ssh"
        containers.append(ssh_name)
        run("docker", "run", "-d", "--name", ssh_name, *common, *ssh_runtime, *main_memory,
            "--mount", f"type=volume,src={public_volume},dst=/host-public,volume-nocopy",
            "-p", "127.0.0.1::1022", args.image, "ssh", env=process_env)
        collector_name = prefix + "-collector"
        containers.append(collector_name)
        run("docker", "run", "-d", "--name", collector_name, *common, *collector_runtime,
            "--mount", f"type=volume,src={public_volume},dst=/host-public,readonly,volume-nocopy",
            "--mount", f"type=volume,src={state_volume},dst=/state",
            "-p", "127.0.0.1::8090", args.image, "collector", env=process_env)

        def mapped_port(name, port):
            state = json.loads(run("docker", "inspect", name).stdout)[0]
            return int(state["NetworkSettings"]["Ports"][f"{port}/tcp"][0]["HostPort"])
        ssh_port = mapped_port(ssh_name, 1022)
        collector_port = mapped_port(collector_name, 8090)
        base = f"http://127.0.0.1:{collector_port}"

        def request_collector(path, body=None, authorization=token):
            headers = {"Content-Type": "application/json"}
            if authorization:
                headers["Authorization"] = "Bearer " + authorization
            request = urllib.request.Request(
                base + path, data=json.dumps(body).encode() if body is not None else None,
                headers=headers,
            )
            try:
                with urllib.request.urlopen(request, timeout=3) as response:
                    return response.status, json.load(response)
            except urllib.error.HTTPError as error:
                return error.code, json.load(error)

        identity = None
        for _ in range(60):
            try:
                status, value = request_collector("/identity")
                if status == 200:
                    identity = value["identity"]
                    break
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.25)
        if identity is None:
            raise RuntimeError("SSH/collector readiness failed")
        assert request_collector("/identity", authorization=None)[0] == 401
        assert request_collector("/identity", authorization="00" * 32)[0] == 401
        known_hosts = directory / "known_hosts"
        key = identity["guest_channel_key"]
        known_hosts.write_text(f'[127.0.0.1]:{ssh_port} {key["algorithm"]} {key["key_base64"]}\n')
        ssh_options = [
            "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={known_hosts}",
            "-o", "ConnectTimeout=3", "-o", "LogLevel=ERROR",
        ]

        def ssh(command, key_name="buyer", check=True, input_text=None):
            return run("ssh", *ssh_options, "-i", str(directory / key_name),
                       "-p", str(ssh_port), "covenant@127.0.0.1", command,
                       check=check, input_text=input_text)
        assert ssh("id -u").stdout.strip() == "10001"
        assert ssh("pwd").stdout.strip() == "/workspace"
        outbound_ssh_smoke(directory, ssh_port, identity, policy)
        # Actual kernel interfaces must meet the same policy in both roles.
        for name, role in ((ssh_name, "ssh"), (collector_name, "collector")):
            run("docker", "exec", name, "python3", "-c",
                f"from memory_guard import RuntimeGuard; RuntimeGuard().check('{role}')")
        status_raw = ssh(f"cat /host-public/{memory.STATUS_NAME}").stdout
        assert json.loads(status_raw)["profile"] == memory.PROFILE
        ssh("cat > /workspace/maximum.snapshot", input_text="x" * (64 * 1024 * 1024))
        assert ssh("wc -c < /workspace/maximum.snapshot").stdout.strip() == "67108864"
        ssh("rm /workspace/maximum.snapshot")
        # Prove the mount cap is enforced rather than merely displayed in
        # Compose. No host file or swap device is created.
        capacity_test = (
            "import errno,os\n"
            "p='/workspace/capacity-test'\n"
            "f=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)\n"
            "try:\n"
            " try: os.posix_fallocate(f,0,129*1024*1024)\n"
            " except OSError as e: assert e.errno==errno.ENOSPC\n"
            " else: raise AssertionError('workspace limit not enforced')\n"
            "finally:\n"
            " os.close(f)\n"
            " os.unlink(p)\n"
        )
        ssh("python3 -", input_text=capacity_test)
        assert ssh("cat /run/covenant/host_key", check=False).returncode != 0
        ssh("test ! -e /var/run/dstack.sock")
        ssh("test ! -w /host-public/host_key.pub")
        ssh(f"test ! -w /host-public/{memory.STATUS_NAME}")
        assert ssh(f"rm /host-public/{memory.STATUS_NAME}", check=False).returncode != 0
        assert ssh(f"printf tampered > /host-public/{memory.STATUS_NAME}", check=False).returncode != 0
        ssh("test ! -w /run/authorized_keys")
        ssh("test ! -w /opt/covenant/collector.py")
        ssh("test -z \"${COVENANT_COLLECTOR_TOKEN+x}\"")
        assert ssh("id", key_name="wrong-buyer", check=False).returncode != 0
        ssh("cat > /workspace/synthetic.txt", input_text="synthetic acceptance only\n")
        assert ssh("cat /workspace/synthetic.txt").stdout == "synthetic acceptance only\n"
        source = directory / "sftp-source"
        source.write_text("synthetic sftp\n")
        batch = directory / "sftp-batch"
        batch.write_text(f"put {source} /workspace/sftp.txt\n")
        run("sftp", *ssh_options, "-i", str(directory / "buyer"),
            "-P", str(ssh_port), "-b", str(batch), "covenant@127.0.0.1")
        assert ssh("cat /workspace/sftp.txt").stdout == "synthetic sftp\n"
        challenge = {
            "schema_version": 1, "challenge_id": str(uuid.uuid4()),
            "nonce_hex": secrets.token_hex(32), "issued_at_ms": int(time.time() * 1000),
            "expires_at_ms": int(time.time() * 1000) + 60_000,
            "scope": {field: policy[field] for field in (
                "job_id", "operator_pubkey_b58", "buyer_pubkey_b58",
                "lease_generation", "lease_expires_at_ms",
            )},
        }
        challenge["scope"].update({field: identity[field] for field in (
            "host", "port", "username", "guest_channel_key", "transport",
        )})
        challenge["scope"]["workload_sha256"] = INFO["compose_hash"]
        wrong = copy.deepcopy(challenge)
        wrong["scope"]["host"] = "unrelated.example"
        assert request_collector("/quote", wrong)[0] == 409
        assert guest_calls == []
        status, evidence = request_collector("/quote", challenge)
        assert status == 200 and evidence["verification_status"] == "unverified"
        assert evidence["dstack_quote"]["event_log"] == EVENTS
        assert len(guest_calls) == 1
        assert request_collector("/quote", challenge)[0] == 409
        run("docker", "restart", collector_name)
        # Docker may allocate a new ephemeral host port across a restart.
        collector_port = mapped_port(collector_name, 8090)
        base = f"http://127.0.0.1:{collector_port}"
        restarted = False
        for _ in range(30):
            try:
                if request_collector("/identity")[0] == 200:
                    restarted = True
                    break
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.1)
        assert restarted, "collector did not become ready after restart"
        assert request_collector("/quote", challenge)[0] == 409
        assert len(guest_calls) == 1
        # Replacing a host-key observation must fail the actual SSH handshake.
        wrong_public = " ".join((directory / "wrong-buyer.pub").read_text().split()[:2])
        known_hosts.write_text(f"[127.0.0.1]:{ssh_port} {wrong_public}\n")
        assert ssh("id", check=False).returncode != 0
        # A restart clears the private key and workspace even while the
        # collector keeps the shared public tmpfs mounted.
        old_key = identity["guest_channel_key"]
        run("docker", "restart", ssh_name)
        ssh_port = mapped_port(ssh_name, 1022)
        changed_identity = None
        for _ in range(60):
            try:
                status, value = request_collector("/identity")
                if status == 200 and value["identity"]["guest_channel_key"] != old_key:
                    changed_identity = value["identity"]
                    break
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.1)
        assert changed_identity is not None, "SSH restart reused key or failed memory checks"
        known_hosts.write_text(
            f'[127.0.0.1]:{ssh_port} {old_key["algorithm"]} {old_key["key_base64"]}\n')
        assert ssh("id", check=False).returncode != 0
        changed_key = changed_identity["guest_channel_key"]
        known_hosts.write_text(
            f'[127.0.0.1]:{ssh_port} {changed_key["algorithm"]} {changed_key["key_base64"]}\n')
        ssh("test ! -e /workspace/synthetic.txt && test ! -e /workspace/sftp.txt")
        # Change a real kernel-enforced control after startup. The watchdog
        # must kill an established session, and collection must stop.
        active = subprocess.Popen(
            ["ssh", *ssh_options, "-i", str(directory / "buyer"), "-p", str(ssh_port),
             "covenant@127.0.0.1", "printf 'ready\\n'; sleep 30; printf should-not-complete"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            assert select.select([active.stdout], [], [], 5)[0], "SSH session did not start"
            assert active.stdout.readline() == "ready\n"
            run("docker", "update", "--memory-swap", "1536m", ssh_name)
            stdout, _ = active.communicate(timeout=10)
            assert active.returncode != 0 and "should-not-complete" not in stdout
        finally:
            if active.poll() is None:
                active.kill()
                active.communicate()
        for _ in range(60):
            running = json.loads(run("docker", "inspect", ssh_name).stdout)[0]["State"]["Running"]
            if not running:
                break
            time.sleep(0.1)
        assert not running, "SSH survived a swap-limit change"
        assert request_collector("/identity")[0] == 503
        assert ssh("id", check=False).returncode != 0
        # Negative startup profiles exercise actual mounts/cgroup controls,
        # never substitute fake /proc data or enable swap on the CI host.
        rejected_public = create_volume("rejected-public", public_options)
        for label, swap_allowed, workspace_noswap in (
            ("swap-allowed", True, True),
            ("swappable-workspace", False, False),
        ):
            rejected_name = prefix + "-" + label
            containers.append(rejected_name)
            rejected_memory = ssh_memory_mounts(label, workspace_noswap)
            rejected_runtime = list(ssh_runtime)
            if swap_allowed:
                rejected_runtime[rejected_runtime.index("--memory-swap") + 1] = "1536m"
            result = run("docker", "run", "--name", rejected_name, *common,
                         *rejected_runtime, *rejected_memory,
                         "--mount", f"type=volume,src={rejected_public},dst=/host-public,volume-nocopy",
                         args.image, "ssh", env=process_env, check=False)
            assert result.returncode != 0, f"unsafe {label} startup was accepted"
            assert "UnsafeRuntime" in result.stderr, "failure was not the runtime guard"
        # A distinct short lease proves process/session shutdown. Never mutate
        # the main lease policy or reuse its persistent consumption journal.
        expiry_policy = copy.deepcopy(policy)
        expiry_policy.update(job_id=str(uuid.uuid4()),
                             lease_expires_at_ms=int(time.time() * 1000) + 20_000)
        expiry_env = dict(process_env, COVENANT_LEASE_POLICY_B64=base64.b64encode(
            c.canonical(expiry_policy)).decode())
        expiry_public = create_volume("expiry-public", public_options)
        expiry_state = create_volume("expiry-state")
        expiry_memory = ssh_memory_mounts("expiry")
        expiry_ssh = prefix + "-expiry-ssh"
        containers.append(expiry_ssh)
        run("docker", "run", "-d", "--name", expiry_ssh, *common, *ssh_runtime, *expiry_memory,
            "--mount", f"type=volume,src={expiry_public},dst=/host-public,volume-nocopy",
            "-p", "127.0.0.1::1022", args.image, "ssh", env=expiry_env)
        expiry_collector = prefix + "-expiry-collector"
        containers.append(expiry_collector)
        run("docker", "run", "-d", "--name", expiry_collector, *common, *collector_runtime,
            "--mount", f"type=volume,src={expiry_public},dst=/host-public,readonly,volume-nocopy",
            "--mount", f"type=volume,src={expiry_state},dst=/state",
            "-p", "127.0.0.1::8090", args.image, "collector", env=expiry_env)
        ssh_port = mapped_port(expiry_ssh, 1022)
        base = f"http://127.0.0.1:{mapped_port(expiry_collector, 8090)}"
        expiry_identity = None
        for _ in range(60):
            try:
                status, value = request_collector("/identity")
                if status == 200:
                    expiry_identity = value["identity"]
                    break
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.1)
        assert expiry_identity is not None
        expiry_key = expiry_identity["guest_channel_key"]
        known_hosts.write_text(
            f'[127.0.0.1]:{ssh_port} {expiry_key["algorithm"]} {expiry_key["key_base64"]}\n')
        assert ssh("id -u").stdout.strip() == "10001"
        session = subprocess.Popen(
            ["ssh", *ssh_options, "-i", str(directory / "buyer"), "-p", str(ssh_port),
             "covenant@127.0.0.1", "sleep 30; printf should-not-complete"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            stdout, _ = session.communicate(timeout=25)
        except subprocess.TimeoutExpired:
            session.kill()
            session.communicate()
            raise AssertionError("existing SSH session survived lease expiry")
        assert session.returncode != 0 and "should-not-complete" not in stdout
        assert ssh("id", check=False).returncode != 0
        for _ in range(30):
            states = [json.loads(run("docker", "inspect", name).stdout)[0]["State"]["Running"]
                      for name in (expiry_ssh, expiry_collector)]
            if not any(states):
                break
            time.sleep(0.1)
        assert not any(states), "lease roles remained running after expiry"
        print("PASS: real SSH/SFTP/auth, tmpfs workspace/key rotation, swap guard/watchdog, replay, expiry.")
        print("No genuine hardware evidence, Phala transport, lease integration or trust accepted.")
    except Exception:
        for name in containers:
            inspected = run("docker", "inspect", name, check=False)
            if inspected.returncode == 0:
                state = json.loads(inspected.stdout)[0]["State"]
                summary = {key: state.get(key) for key in ("Status", "ExitCode", "OOMKilled")}
                print(f"{name} state: {json.dumps(summary, sort_keys=True)}")
            logs = run("docker", "logs", "--tail", "30", name, check=False)
            print(f"{name} diagnostic logs:\n{logs.stdout}{logs.stderr}")
        raise
    finally:
        for name in reversed(containers):
            run("docker", "rm", "-f", name, check=False)
        for name in reversed(volumes):
            run("docker", "volume", "rm", name, check=False)
        if guest_server is not None:
            guest_server.shutdown()
            guest_server.server_close()
        directory_context.cleanup()


if __name__ == "__main__":
    main()
