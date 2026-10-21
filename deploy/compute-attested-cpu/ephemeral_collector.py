"""Explicit no-KMS profile; outbound routing never grants hardware trust."""
import http.client
import json
import sys
import time
from urllib.parse import urlsplit

import collector
from memory_guard import RuntimeGuard

PROFILE = "phala_ephemeral_dstack061"


class EphemeralCollector(collector.Collector):
    def check_stream(self, identity):
        collector.require(self.clock() < self.policy["lease_expires_at_ms"],
                          "expired stream lease")
        with self.key_path.open("rb") as public_file:
            raw = public_file.read(257)
        collector.require(len(raw) <= 256, "oversized SSH key")
        public = raw.decode("ascii")
        collector.require(collector.ssh_key(public) == identity["guest_channel_key"],
                          "stream SSH key changed")
        self.memory_guard.check_peer(self.policy, public)

    def endpoint(self, info):
        collector.require(self.policy.get("profile") == PROFILE,
                          "ephemeral profile required")
        provider = collector.strict_json(info["vm_config"]).get("key_provider")
        # Info is an unauthenticated consistency hint. The independent verifier
        # checks the modern key-provider RTMR event and exact measured workload.
        if provider is not None:
            collector.require(provider == {"name": "none", "id": ""},
                              "unexpected ephemeral key provider")
        return {"host": urlsplit(self.policy["relay_endpoint"]).hostname,
                "port": 443, "transport": {"kind": "outbound_relay"}}

    def scope_fields(self):
        return collector.SCOPE_FIELDS | {"profile"}

    def validate(self, challenge, identity, now_ms):
        collector.require(challenge.get("scope", {}).get("profile") == PROFILE,
                          "wrong hardware verification profile")
        return super().validate(challenge, identity, now_ms)


def main():
    from outbound_worker import OutboundWorker
    policy = collector.environment_policy()
    collector.require(policy.get("profile") == PROFILE, "ephemeral policy required")
    RuntimeGuard().check("collector")
    journal = collector.ReplayStore("/state/challenges.sqlite3", policy["job_id"])
    evidence = EphemeralCollector(policy, "/host-public/host_key.pub",
                                  collector.Dstack(), journal)
    # SSH starts independently. Waiting is bounded by the immutable lease;
    # no registration or quote is emitted until memory checks pass.
    ready_deadline = time.monotonic() + min(
        180, (policy["lease_expires_at_ms"] - int(time.time() * 1000)) / 1000)
    last_notice = 0
    while int(time.time() * 1000) < policy["lease_expires_at_ms"] and time.monotonic() < ready_deadline:
        try:
            evidence.identity()
            break
        except (OSError, ValueError, http.client.HTTPException) as error:
            if time.monotonic() - last_notice >= 10:
                print(json.dumps({"event": "outbound_waiting_for_guarded_ssh",
                                  "error_class": type(error).__name__}), file=sys.stderr)
                last_notice = time.monotonic()
            time.sleep(0.25)
    else:
        raise collector.Rejected("guarded SSH readiness expired")
    OutboundWorker(policy["relay_endpoint"], policy["relay_session_id"],
                   evidence, policy["lease_expires_at_ms"],
                   ssh_address=("ssh", 1022)).run()
