#!/usr/bin/env python3
"""Subprocess contract tests: these never contact a coordinator or rent a GPU."""

import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import shutil
import time


CANARY = Path(__file__).resolve().parents[1] / "compute-rental-canary.py"
SPEC = importlib.util.spec_from_file_location("rental_canary", CANARY)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

FAKE = r'''#!/usr/bin/env python3
import base64, json, os, pathlib, sys, time
root = pathlib.Path(os.environ["CANARY_FIXTURE"])
scenario = os.environ.get("CANARY_SCENARIO", "success")
state_path = root / "fake-state.json"
state = json.loads(state_path.read_text()) if state_path.exists() else {"opens": 0, "closed": False}
job = state.get("job", "d62dcd2e-d89e-4854-b8c1-1d56c8419cbd")
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as log: log.write(json.dumps(args) + "\n")
def save(): state_path.write_text(json.dumps(state))
def emit(value): print(json.dumps(value))
def view():
    return {"job_id": job, "status": ("refunded" if scenario == "refunded" else "completed") if state["closed"] else "accepted",
            "access": {"endpoint": "ssh root@gpu.example.com -p 2201"},
            "charged_micro_usdc": 0 if scenario == "refunded" or not state["closed"] else 37}
if args and args[0] == "evidence":
    request = json.load(sys.stdin)
    op = request["operation"]
    out = {"version": 1, "operation": op, "job_id": request.get("job_id"), "observed_at_unix": time.time(), "provider_instance_id": "vast:123"}
    if op == "preflight":
        out.update(host_key_binding="missing" if scenario == "missing_binding" else "authenticated", cleanup="provider_readback", provider_limits_checked=True, provider_reservation_micro_usd=350000)
    elif op == "access":
        out.update(binding={"verified": scenario != "unverified_host", "source": "operator_signed_ready", "job_id": request["job_id"]},
                   ssh={"host": "other.example.com" if scenario == "wrong_host" else "gpu.example.com", "port": 2201, "user": "root", "public_key": "ssh-ed25519 " + base64.b64encode(b"a" * 64).decode()})
        if scenario == "stale_evidence": out["observed_at_unix"] -= 1000
    elif op == "cleanup":
        out.update(binding_verified=True, source="provider_readback", exists=scenario == "cleanup_failure")
        if scenario == "wrong_cleanup": out["provider_instance_id"] = "vast:other"
        if scenario == "provider_overrun": out.update(provider_cost_verified=True, provider_cost_micro_usd=800000)
    emit(out)
elif args and args[0] == "cli":
    args = [arg for arg in args[1:] if arg != "--json"]
    if args == ["--version"]: print("covenant-compute 0.3.1")
    elif args[0] == "capacity":
        emit({"entries": [] if scenario == "no_capacity" else [{"kind": "lease_session", "operators": 1, "max_vram_gb": 24, "min_ask_unit": "per_gpu_second", "min_ask_micro_usdc": 1, "gpu_classes": ["rtx-4090"]}]})
    elif args[0] == "balance":
        charged = 0 if not state["closed"] or scenario == "refunded" else 37
        deposited = 0 if scenario in ("underfunded", "explicit_deposit") and not state.get("deposit") else 10000
        emit({"balance": {"deposited_micro_usdc": deposited, "withdrawn_micro_usdc": 0, "charged_micro_usdc": charged, "available_micro_usdc": deposited - charged - (100 if scenario == "refund_missing" and state["closed"] else 0)}})
    elif args[0] == "deposit":
        state["deposit"] = True; save(); emit({"credited": True})
    elif args[:2] == ["lease", "open"]:
        job = args[args.index("--job-id") + 1]
        state["job"] = job
        # The monetary reservation and recovery ID already exist before dispatch.
        ledger = json.loads((root / "state" / "budget.json").read_text())
        assert ledger["entries"][-1]["job_id"] == job
        assert ledger["entries"][-1]["counted_micro_usd"] == 371300
        state["opens"] += 1; save()
        if scenario == "ambiguous_launch": sys.exit(1)
        if scenario == "lost_launch_response":
            print("lease " + job + " opened — 300 escrowed", file=sys.stderr); sys.exit(1)
        emit(view())
    elif args[:2] == ["lease", "close"]:
        if scenario == "ambiguous_launch": sys.exit(8)
        state["closed"] = True; save(); emit(view())
    elif args[:2] == ["lease", "view"]: emit(view())
    elif args[0] == "verify":
        emit({"job_id": job, "verdict": "offchain_record_only" if scenario == "offchain_payout" else "no_receipt" if scenario == "refunded" else "verified_onchain"})
    elif args[0] == "receipts": emit([{"job_id": job, "status": "refunded", "charged_micro_usdc": 0, "receipt_verified": None}])
    else: sys.exit(5)
else:
    assert "StrictHostKeyChecking=yes" in args
    assert "GlobalKnownHostsFile=/dev/null" in args
    assert "BatchMode=yes" in args
    assert args[-1] == "nvidia-smi --query-gpu=uuid,name,memory.total --format=csv,noheader,nounits"
    known = next(arg.split("=", 1)[1] for arg in args if arg.startswith("UserKnownHostsFile="))
    assert "ssh-ed25519" in pathlib.Path(known).read_text()
    if scenario == "probe_failure": sys.exit(7)
    if scenario == "probe_timeout": time.sleep(10)
    print("GPU-fake-test-device, RTX 4090, 24576")
'''


class CanarySubprocessTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.fake = self.root / "fake"
        self.fake.write_text(FAKE)
        self.fake.chmod(0o700)
        (self.root / "buyer").mkdir()
        (self.root / "buyer" / "identity.json").write_bytes(bytes(range(32)))
        (self.root / "public").mkdir()
        (self.root / "id").write_text("test-only")
        (self.root / "id.pub").write_text("test-only")
        self.config = {
            "coordinator_url": "https://coordinator.example.com", "rpc_url": "https://rpc.example.com",
            "buyer_home": str(self.root / "buyer"), "ssh_private_key": str(self.root / "id"),
            "ssh_public_key": str(self.root / "id.pub"), "ssh_binary": str(self.fake),
            "cli": [sys.executable, str(self.fake), "cli"],
            "evidence_command": [sys.executable, str(self.fake), "evidence"],
            "max_spend_micro_usdc": 300, "rate_micro_usdc_per_sec": 100, "duration_secs": 3,
            "daily_limit_micro_usd": 2000000, "pilot_limit_micro_usd": 20000000,
            "provider_reservation_micro_usd": 350000, "fee_reservation_micro_usd": 20000, "storage_reservation_micro_usd": 1000,
            "expected_cli_version": "0.3.1", "expected_gpu_model": "RTX 4090", "min_gpu_memory_mib": 1024,
            "public_state_path": str(self.root / "public" / "state.json"),
            "readiness_timeout_secs": 1, "settlement_timeout_secs": 1, "cleanup_timeout_secs": 1,
            "command_timeout_secs": 1, "poll_secs": 1,
        }

    def tearDown(self):
        self.temporary.cleanup()

    def run_canary(self, scenario="success", preflight=False, recover_only=False):
        path = self.root / "config.json"
        path.write_text(json.dumps(self.config))
        result = subprocess.run([sys.executable, str(CANARY), "--config", str(path),
                                 "--state-dir", str(self.root / "state")] + (["--preflight"] if preflight else []) + (["--recover-only"] if recover_only else []),
                                env={**os.environ, "CANARY_FIXTURE": str(self.root), "CANARY_SCENARIO": scenario},
                                capture_output=True, text=True, timeout=15)
        self.assertTrue(result.stdout, result.stderr)
        verdict = json.loads(result.stdout)
        durable = json.loads((self.root / "state" / "runs" / (verdict["run_id"] + ".json")).read_text())
        self.assertEqual(verdict, durable)
        return result.returncode, verdict

    def calls(self):
        path = self.root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def opened(self):
        return sum(call[:3] == ["cli", "lease", "open"] for call in self.calls())

    def test_complete_remote_cli_contract_and_strict_ssh(self):
        code, verdict = self.run_canary()
        self.assertEqual(code, 0, verdict)
        self.assertEqual(verdict["verdict"], "success")
        self.assertEqual(verdict["charged_micro_usdc"], 37)
        self.assertEqual(verdict["refunded_micro_usdc"], 263)
        self.assertTrue(verdict["provider_cleanup_verified"])
        self.assertTrue(verdict["balance_reconciled"])
        self.assertFalse((self.root / "state" / "active.json").exists())
        self.assertEqual(self.opened(), 1)
        self.assertFalse(list((self.root / "state").glob("known-hosts-*")))

    def test_preflight_is_never_rental_success_or_spend(self):
        code, verdict = self.run_canary(preflight=True)
        self.assertEqual((code, verdict["verdict"]), (2, "preflight_only"))
        self.assertEqual(self.opened(), 0)
        self.assertFalse(any(call[:2] == ["cli", "deposit"] for call in self.calls()))

    def test_no_evidence_or_no_funds_blocks_before_spending(self):
        for scenario in ("missing_binding", "underfunded", "no_capacity"):
            with self.subTest(scenario=scenario):
                code, verdict = self.run_canary(scenario)
                self.assertEqual((code, verdict["verdict"]), (2, "blocked"))
        self.assertEqual(self.opened(), 0)

    def test_only_explicit_existing_deposit_is_claimed(self):
        self.config["deposit_signature"] = "A" * 88
        code, verdict = self.run_canary("explicit_deposit")
        self.assertEqual(code, 0, verdict)
        self.assertEqual(sum(call[:2] == ["cli", "deposit"] for call in self.calls()), 1)

    def test_limit_refuses_before_subprocess(self):
        self.config["max_spend_micro_usdc"] = 299
        code, verdict = self.run_canary()
        self.assertEqual(verdict["reason"], "lease_exceeds_max_spend")
        self.assertEqual(code, 2)
        self.assertEqual(self.calls(), [])

    def test_unverified_ssh_is_not_run_and_rental_still_closes(self):
        code, verdict = self.run_canary("unverified_host")
        self.assertEqual((code, verdict["verdict"]), (2, "blocked"))
        self.assertEqual(verdict["reason"], "host_key_binding_unverified")
        self.assertTrue(verdict["provider_cleanup_verified"])
        self.assertFalse(any(call and call[0] == "-F" for call in self.calls()))

    def test_probe_failure_and_timeout_close_verify_and_cleanup(self):
        for scenario in ("probe_failure", "probe_timeout"):
            with self.subTest(scenario=scenario):
                # A fresh provider fixture for each separately paid run.
                (self.root / "fake-state.json").unlink(missing_ok=True)
                shutil.rmtree(self.root / "state", ignore_errors=True)
                code, verdict = self.run_canary(scenario)
                self.assertEqual(code, 1, verdict)
                self.assertEqual(verdict["reason"], "gpu_probe_failed")
                self.assertTrue(verdict["provider_cleanup_verified"])
                self.assertEqual(verdict["settlement_verdict"], "verified_onchain")

    def test_ambiguous_launch_is_durable_and_never_rebought(self):
        code, verdict = self.run_canary("ambiguous_launch")
        self.assertEqual(verdict["reason"], "launch_outcome_unknown_do_not_retry")
        self.assertEqual(code, 1)  # unresolved reconciliation takes precedence
        active = json.loads((self.root / "state" / "active.json").read_text())
        self.assertTrue(active["job_id"])
        code, verdict = self.run_canary()
        self.assertEqual(verdict["reason"], "prior_rental_reconciled")
        self.assertEqual(verdict["verdict"], "recovered")
        self.assertEqual(self.opened(), 1)

    def test_stderr_job_id_recovers_failed_open_response(self):
        code, verdict = self.run_canary("lost_launch_response")
        self.assertEqual(code, 1)
        self.assertEqual(verdict["reason"], "launch_response_failed")
        self.assertTrue(verdict["provider_cleanup_verified"])
        self.assertFalse((self.root / "state" / "active.json").exists())

    def test_cleanup_failure_blocks_new_rental_then_recovers_without_buying(self):
        code, verdict = self.run_canary("cleanup_failure")
        self.assertEqual(code, 1)
        self.assertTrue((self.root / "state" / "active.json").exists())
        code, verdict = self.run_canary()
        self.assertEqual(code, 0)
        self.assertTrue(verdict["recovery_only"])
        self.assertTrue(verdict["provider_cleanup_verified"])
        self.assertEqual(self.opened(), 1)
        self.assertFalse((self.root / "state" / "active.json").exists())

    def test_offchain_payout_never_passes(self):
        code, verdict = self.run_canary("offchain_payout")
        self.assertEqual(code, 1)
        self.assertIn("payout_not_verified_onchain", verdict["reconciliation_errors"])
        self.assertTrue(verdict["provider_cleanup_verified"])
        self.assertTrue((self.root / "state" / "active.json").exists())

    def test_refund_is_reconciled_but_not_successful_rental(self):
        code, verdict = self.run_canary("refunded")
        self.assertNotEqual(code, 0)
        self.assertEqual(verdict["settlement_verdict"], "refunded_balance_reconciliation")
        self.assertEqual(verdict["refunded_micro_usdc"], 300)
        self.assertTrue(verdict["balance_reconciled"])

    def test_missing_refund_fails(self):
        code, verdict = self.run_canary("refund_missing")
        self.assertIn("escrow_refund_not_reconciled", verdict["reconciliation_errors"])
        self.assertEqual(code, 1)

    def test_wrong_provider_identity_never_confirms_cleanup(self):
        code, verdict = self.run_canary("wrong_cleanup")
        self.assertEqual(code, 1)
        self.assertIn("cleanup_instance_mismatch", verdict["reconciliation_errors"])
        self.assertTrue((self.root / "state" / "active.json").exists())

    def test_failed_recheck_cannot_inherit_old_settlement_flag(self):
        self.run_canary("cleanup_failure")
        code, verdict = self.run_canary("offchain_payout")
        self.assertEqual(code, 1)
        self.assertIn("payout_not_verified_onchain", verdict["reconciliation_errors"])
        self.assertTrue((self.root / "state" / "active.json").exists())

    def test_invalid_configuration_still_writes_verdict(self):
        del self.config["evidence_command"]
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["verdict"]), (2, "blocked"))
        self.assertEqual(verdict["reason"], "invalid_evidence_command")

    def test_concurrent_run_has_own_verdict_and_does_not_spend(self):
        directory = self.root / "state"
        directory.mkdir()
        with (directory / "run.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "another_canary_is_running"))
        self.assertEqual(self.opened(), 0)

    def test_local_coordinator_is_rejected(self):
        self.config["coordinator_url"] = "https://127.0.0.1:8720"
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "local_coordinator_url"))

    def test_same_window_never_buys_twice(self):
        self.run_canary()
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "window_already_reserved"))
        self.assertEqual(self.opened(), 1)

    def test_missing_active_file_recovers_from_monetary_journal(self):
        self.run_canary("cleanup_failure")
        (self.root / "state" / "active.json").unlink()
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["verdict"]), (0, "recovered"))
        self.assertEqual(self.opened(), 1)

    def test_recovery_only_never_opens_new_rental_when_nothing_pending(self):
        code, verdict = self.run_canary(recover_only=True)
        self.assertEqual((code, verdict["reason"]), (2, "no_unresolved_rental"))
        self.assertEqual(self.calls(), [])

    def test_stale_secondary_record_cannot_turn_recovery_into_new_purchase(self):
        self.run_canary()
        journal = json.loads((self.root / "state" / "budget.json").read_text())
        active = journal["entries"][0]["active"]
        (self.root / "state" / "active.json").write_text(json.dumps(active))
        code, verdict = self.run_canary(recover_only=True)
        self.assertEqual((code, verdict["reason"]), (2, "no_unresolved_rental"))
        self.assertEqual(self.opened(), 1)
        self.assertFalse((self.root / "state" / "active.json").exists())

    def test_unjournaled_secondary_active_record_blocks_new_spend(self):
        self.run_canary(preflight=True)
        (self.root / "state" / "active.json").write_text(json.dumps({"job_id": "unknown"}))
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "orphan_active_record"))
        self.assertEqual(self.opened(), 0)

    def test_missing_budget_never_reinitializes_pilot(self):
        self.run_canary()
        (self.root / "state" / "budget.json").unlink()
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "budget_journal_missing"))
        self.assertEqual(self.opened(), 1)

    def test_public_state_has_no_private_evidence(self):
        code, verdict = self.run_canary()
        self.assertEqual(code, 0, verdict)
        path = self.root / "public" / "state.json"
        value = json.loads(path.read_text())
        self.assertEqual(value["schema"], 1)
        self.assertEqual(value["latest"]["verdict"], "success")
        self.assertTrue(value["latest"]["ssh_identity_verified"])
        self.assertEqual(value["budget"]["pilot_reserved_micro_usd"], 371037)
        self.assertEqual(path.stat().st_mode & 0o777, 0o644)
        for secret in ("gpu.example.com", "vast:123", "ssh-ed25519", str(self.root), "balance_before"):
            self.assertNotIn(secret, path.read_text())

    def test_attributable_provider_overrun_fails_and_stops_pilot(self):
        code, verdict = self.run_canary("provider_overrun")
        self.assertEqual((code, verdict["reason"]), (1, "provider_budget_breach"))
        state = json.loads((self.root / "public" / "state.json").read_text())
        self.assertFalse(state["enabled"])
        self.assertEqual(state["budget"]["pilot_reserved_micro_usd"], 821037)
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "provider_budget_breach"))
        self.assertEqual(self.opened(), 1)

    def test_version_or_gpu_model_drift_cannot_pass(self):
        self.config["expected_cli_version"] = "0.3.0"
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "buyer_version_unavailable"))
        self.assertEqual(self.opened(), 0)
        self.config["expected_cli_version"] = "0.3.1"
        self.config["expected_gpu_model"] = "NVIDIA L40S"
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (1, "gpu_probe_invalid"))

    def test_hard_admission_limits_cannot_be_raised(self):
        self.config["daily_limit_micro_usd"] = 2000001
        code, verdict = self.run_canary()
        self.assertEqual(code, 2)
        self.assertEqual(self.calls(), [])

    def test_replaced_buyer_seed_refuses_before_any_cli_call(self):
        self.run_canary(preflight=True)
        count = len(self.calls())
        (self.root / "buyer" / "identity.json").write_bytes(b"x" * 32)
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "budget_identity_or_limits_changed"))
        self.assertEqual(len(self.calls()), count)

    def test_missing_or_symlinked_seed_is_rejected(self):
        seed = self.root / "buyer" / "identity.json"
        seed.unlink()
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "invalid_buyer_identity"))
        target = self.root / "other-seed"
        target.write_bytes(b"x" * 32)
        seed.symlink_to(target)
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "invalid_buyer_identity"))
        self.assertEqual(self.calls(), [])

    def test_exhausted_pilot_and_changed_limits_never_dispatch(self):
        self.run_canary()
        path = self.root / "state" / "budget.json"
        journal = json.loads(path.read_text())
        journal["entries"][0]["counted_micro_usd"] = 20000000
        path.write_text(json.dumps(journal))
        call_count = len(self.calls())
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "pilot_budget_exhausted"))
        self.assertEqual(len(self.calls()), call_count)
        self.config["pilot_limit_micro_usd"] = 19000000
        code, verdict = self.run_canary()
        self.assertEqual((code, verdict["reason"]), (2, "budget_identity_or_limits_changed"))

    def test_monitor_does_not_take_rental_lock_or_dispatch(self):
        self.run_canary(preflight=True)
        directory = self.root / "state"
        journal = json.loads((directory / "budget.json").read_text())
        journal["created_at_unix"] = (int(time.time()) // MODULE.WINDOW_SECONDS - 3) * MODULE.WINDOW_SECONDS
        (directory / "budget.json").write_text(json.dumps(journal))
        with (directory / "run.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            state = MODULE.monitor_once(directory, MODULE.load_config(self.root / "config.json"))
        self.assertTrue(state["monitor"]["alert_due"])
        self.assertEqual(self.opened(), 0)

    def test_monitor_alerts_on_missing_windows_once_then_reports_recovery(self):
        self.run_canary(preflight=True)
        directory = self.root / "state"
        journal = json.loads((directory / "budget.json").read_text())
        now = int(time.time())
        journal["created_at_unix"] = (now // MODULE.WINDOW_SECONDS - 3) * MODULE.WINDOW_SECONDS
        (directory / "budget.json").write_text(json.dumps(journal))
        alert = self.root / "alert.py"
        output = self.root / "alerts.jsonl"
        alert.write_text("import sys,json\nfrom pathlib import Path\npayload=json.load(sys.stdin)\nwith Path(sys.argv[1]).open('a') as f:f.write(json.dumps(payload)+'\\n')\n")
        config = MODULE.load_config(self.root / "config.json")
        config["alert_command"] = [sys.executable, str(alert), str(output)]
        MODULE.monitor_once(directory, config)
        MODULE.monitor_once(directory, config)
        self.assertEqual(len(output.read_text().splitlines()), 1)
        journal["last_success_window"] = now // MODULE.WINDOW_SECONDS
        journal["last_success_at_unix"] = now
        (directory / "budget.json").write_text(json.dumps(journal))
        MODULE.monitor_once(directory, config)
        rows = [json.loads(line) for line in output.read_text().splitlines()]
        self.assertEqual([row["type"] for row in rows], ["rental_canary_missing_or_unsuccessful", "rental_canary_recovered"])
        self.assertEqual(self.opened(), 0)


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        (self.directory / "buyer").mkdir()
        (self.directory / "buyer" / "identity.json").write_bytes(bytes(range(32)))
        self.config = {"coordinator_url": "https://compute.example.com", "buyer_home": str(self.directory / "buyer"),
                       "daily_limit_micro_usd": 2000000, "pilot_limit_micro_usd": 20000000,
                       "lease_ceiling": 120240, "provider_reservation_micro_usd": 350000,
                       "fee_reservation_micro_usd": 20000, "storage_reservation_micro_usd": 1000,
                       "operating_ceiling": 491240}
        self.budget = MODULE.Budget(self.directory, self.config)

    def tearDown(self):
        self.temporary.cleanup()

    def active(self, number):
        return {"job_id": "00000000-0000-4000-8000-" + str(number).zfill(12), "run_id": "run-" + str(number)}

    def test_unresolved_reservation_never_ages_out(self):
        self.budget.reserve(self.active(1), now=100000)
        totals = self.budget.totals(now=100000 + 100 * MODULE.DAY_SECONDS)
        self.assertEqual(totals["daily_reserved_micro_usd"], 491240)
        with self.assertRaisesRegex(MODULE.CanaryError, "unresolved_rental"):
            self.budget.reserve(self.active(2), now=100000 + 100 * MODULE.DAY_SECONDS)

    def test_rolling_window_uses_resolution_and_never_offsets_operator_revenue(self):
        self.budget.reserve(self.active(1), now=100000)
        with patch.object(MODULE.time, "time", return_value=200000):
            self.budget.resolve(self.active(1), 10000)
        self.assertEqual(self.budget.totals(200000 + MODULE.DAY_SECONDS - 1)["daily_reserved_micro_usd"], 381000)
        self.assertEqual(self.budget.totals(200000 + MODULE.DAY_SECONDS)["daily_reserved_micro_usd"], 0)
        self.assertEqual(self.budget.totals(999999)["pilot_reserved_micro_usd"], 381000)

    def test_provider_overrun_latches_and_increases_consumption(self):
        self.budget.reserve(self.active(1), now=100000)
        self.budget.resolve(self.active(1), 10000, provider_cost=800000)
        self.assertEqual(self.budget.value["latched_reason"], "provider_budget_breach")
        self.assertEqual(self.budget.totals()["pilot_reserved_micro_usd"], 831000)
        with self.assertRaisesRegex(MODULE.CanaryError, "provider_budget_breach"):
            self.budget.reserve(self.active(2), now=999999)

    def test_rolling_daily_ceiling_rejects_fifth_run_before_reservation(self):
        for number in range(1, 5):
            self.budget.reserve(self.active(number), now=number * MODULE.WINDOW_SECONDS)
            # Delayed settlements remain in the same rolling24h window.
            with patch.object(MODULE.time, "time", return_value=10 * MODULE.WINDOW_SECONDS):
                self.budget.resolve(self.active(number), 120240)
        with self.assertRaisesRegex(MODULE.CanaryError, "daily_budget_exhausted"):
            self.budget.reserve(self.active(5), now=11 * MODULE.WINDOW_SECONDS)
        self.assertEqual(len(self.budget.value["entries"]), 4)


class WindowTests(unittest.TestCase):
    def test_authenticated_provider_endpoint_spellings(self):
        for value in ("ssh root@ssh7.vast.ai -p 37974", "ssh -p 37974 root@ssh7.vast.ai", "ssh -p37974 root@ssh7.vast.ai", "ssh root@ssh7.vast.ai -p37974"):
            self.assertEqual(MODULE.endpoint(value), ("ssh7.vast.ai", 37974, "root"))
        for value in ("ssh -p37974 -oProxyCommand=evil root@ssh7.vast.ai", "ssh -p 22 root@x;echo hi", "ssh -p22 root@host -v"):
            with self.assertRaises(MODULE.CanaryError):
                MODULE.endpoint(value)

    def test_durable_success_anchor_survives_history_pruning(self):
        unit = MODULE.WINDOW_SECONDS
        state = MODULE.monitor_state([], 10 * unit, 100 * unit, last_success_window=99, last_success_at=99 * unit)
        self.assertEqual(state["consecutive_unsuccessful_windows"], 0)
        self.assertEqual(state["last_success_at_unix"], 99 * unit)

    def test_monitor_counts_three_absent_windows_and_success_resets(self):
        unit = MODULE.WINDOW_SECONDS
        state = MODULE.monitor_state([], 10 * unit, 13 * unit)
        self.assertEqual(state["consecutive_unsuccessful_windows"], 3)
        self.assertTrue(state["alert_due"])
        state = MODULE.monitor_state([{"started_at_unix": 13 * unit, "finished_at_unix": 13 * unit + 30, "verdict": "success"}], 10 * unit, 13 * unit + 60)
        self.assertFalse(state["alert_due"])
        self.assertEqual(state["consecutive_unsuccessful_windows"], 0)

    def test_recovery_is_not_a_successful_gpu_window(self):
        unit = MODULE.WINDOW_SECONDS
        state = MODULE.monitor_state([{"started_at_unix": 12 * unit, "finished_at_unix": 12 * unit + 30, "verdict": "recovered"}], 10 * unit, 12 * unit + 60)
        self.assertTrue(state["alert_due"])
    def test_three_windows_not_three_invocations_and_success_resets(self):
        unit = MODULE.WINDOW_SECONDS
        state = MODULE.alert_state({}, "failed", 10 * unit)
        for _ in range(4):
            state = MODULE.alert_state(state, "blocked", 10 * unit + 30)
        self.assertEqual(state["failure_windows"], 1)
        self.assertFalse(state["alert_due"])
        state = MODULE.alert_state(state, "failed", 11 * unit)
        self.assertFalse(state["alert_due"])
        state = MODULE.alert_state(state, "failed", 12 * unit)
        self.assertTrue(state["alert_due"])
        state = MODULE.alert_state(state, "success", 12 * unit + 100)
        self.assertEqual(state["failure_windows"], 0)
        self.assertFalse(state["alert_due"])

    def test_preflight_does_not_clear_failed_windows(self):
        state = {"window": 10, "failure_windows": 2, "alert_due": False}
        self.assertEqual(MODULE.alert_state(state, "preflight_only", 11 * MODULE.WINDOW_SECONDS), state)

    def test_unsafe_endpoint_options_are_rejected(self):
        with self.assertRaises(MODULE.CanaryError):
            MODULE.endpoint("ssh -oProxyCommand=evil root@gpu.example.com -p 22")


if __name__ == "__main__":
    unittest.main()
