"""Exercise hostile unmeasured inputs without executing guest system changes."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class EphemeralPrelaunchInputTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        script = Path(__file__).with_name("ephemeral-pre-launch.sh").read_text()
        cls.functions = script.split("\ncovenant_prelaunch_main() {", 1)[0]

    def check_config(self, value):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "user-config.json"
            path.write_text(json.dumps(value))
            return subprocess.run(
                ["bash", "-c", self.functions + '\ncovenant_check_user_config "$1"',
                 "prelaunch-input-test", str(path)],
                capture_output=True, text=True, check=False,
            )

    def test_account_root_credentials_are_rejected(self):
        for value in (
            {"ssh_authorized_keys": "ssh-ed25519 attacker"},
            {"ssh_authorized_keys": ["attacker"]},
            {"root_password": "test-private-value"},
            {"root_public_key": "attacker"},
            ["not an object"],
        ):
            with self.subTest(value=value):
                result = self.check_config(value)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("test-private-value", result.stdout + result.stderr)

    def test_empty_preferences_are_accepted_without_importing_shell_content(self):
        for value in ({}, {"ssh_authorized_keys": ""}, {"ssh_authorized_keys": []}):
            self.assertEqual(self.check_config(value).returncode, 0)
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "must-not-exist"
            self.assertEqual(self.check_config({
                "default_gateway_domain": f"$(touch {marker})",
                "opaque_command": f"touch {marker}",
            }).returncode, 0)
            self.assertFalse(marker.exists())

    def test_inherited_root_credentials_refuse_without_echoing_values(self):
        for name in ("DSTACK_ROOT_PASSWORD", "DSTACK_ROOT_PUBLIC_KEY", "DSTACK_AUTHORIZED_KEYS"):
            env = os.environ.copy()
            env[name] = "test-private-value"
            result = subprocess.run(
                ["bash", "-c", self.functions + "\ncovenant_check_root_env"],
                capture_output=True, text=True, check=False, env=env,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("test-private-value", result.stdout + result.stderr)

    def check_unit(self, unit, states, show_status=0, mask_status=0):
        # Only a shell function is exercised; no host systemctl can run.
        mock = r'''
systemctl() {
    printf '%s\n' "$*" >> "$MOCK_CALLS"
    case "$1" in
        show)
            [[ "$#" == 5 && "$2" == --all && "$4" == --property=ActiveState && "$5" == --value ]] || return 80
            [[ "$3" != *@.service ]] || return 81
            [[ "$MOCK_SHOW_STATUS" == 0 ]] || return "$MOCK_SHOW_STATUS"
            printf '%s' "$MOCK_STATES"
            ;;
        mask)
            [[ "$#" == 3 && "$2" == --runtime ]] || return 82
            return "$MOCK_MASK_STATUS"
            ;;
        *) return 83 ;;
    esac
}
'''
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory) / "calls"
            env = dict(os.environ, MOCK_CALLS=str(calls), MOCK_STATES=states,
                       MOCK_SHOW_STATUS=str(show_status), MOCK_MASK_STATUS=str(mask_status))
            result = subprocess.run(
                ["bash", "-c", self.functions + mock + '\ncovenant_check_login_unit "$1"',
                 "prelaunch-unit-test", unit],
                capture_output=True, text=True, check=False, env=env,
            )
            return result, calls.read_text().splitlines()

    def test_template_queries_instances_and_still_masks_the_template(self):
        for unit in ("ssh@.service", "sshd@.service", "getty@.service", "serial-getty@.service",
                     "container-getty@.service", "autovt@.service"):
            for states in ("", "inactive", "inactive\n\ninactive\n"):
                with self.subTest(unit=unit, states=states):
                    result, calls = self.check_unit(unit, states)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(calls, [
                        f"show --all {unit.replace('@.', '@*.')} --property=ActiveState --value",
                        f"mask --runtime {unit}",
                    ])

    def test_login_states_and_query_errors_fail_closed(self):
        for states in ("active", "inactive\nactive", "activating", "failed", "unknown", " "):
            result, calls = self.check_unit("ssh@.service", states)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(len(calls), 1)
            self.assertIn("unexpected host login instance state", result.stdout)
        for states in ("", "inactive\ninactive", "active"):
            result, calls = self.check_unit("ssh.service", states)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(len(calls), 1)
        result, _ = self.check_unit("ssh.service", "inactive")
        self.assertEqual(result.returncode, 0)
        result, calls = self.check_unit("ssh@.service", "", show_status=9)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("cannot inspect host login unit", result.stdout)
        result, calls = self.check_unit("ssh@.service", "", mask_status=9)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 2)
        self.assertIn("cannot mask host login unit", result.stdout)

    def test_error_trap_reports_only_static_phase_line_and_status_on_stdout(self):
        result = subprocess.run(
            ["bash", "-c", self.functions + r'''
COVENANT_PRELAUNCH_PHASE=host-login-checks
private_value=test-private-value
trap 'covenant_prelaunch_error "$?" "$LINENO"' ERR
false
'''], capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 1)
        self.assertRegex(result.stdout, r"phase=host-login-checks line=[0-9]+ status=1")
        self.assertNotIn("test-private-value", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
