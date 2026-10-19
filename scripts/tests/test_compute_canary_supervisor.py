#!/usr/bin/env python3
"""Supervisor/installer tests. No network, credentials, users, or rentals created."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch, Mock

PATH = Path(__file__).resolve().parents[2] / 'deploy/compute-canary-supervisor.py'
SPEC = importlib.util.spec_from_file_location('canary_supervisor', PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class TemporaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.owner = types.SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid())

    def tearDown(self):
        self.temporary.cleanup()


class InstallerTests(TemporaryTests):
    def archive(self, extra=(), executable=True):
        archive = self.root / 'client.tar.gz'
        with tarfile.open(archive, 'w:gz') as bundle:
            if executable:
                member = tarfile.TarInfo(MODULE.ASSET + '/covenant-compute')
                payload = b'fixture executable bytes'
                member.size = len(payload)
                bundle.addfile(member, io.BytesIO(payload))
            for member in extra:
                bundle.addfile(member, io.BytesIO(b'') if member.isfile() else None)
        return archive, hashlib.sha256(archive.read_bytes()).hexdigest()

    def test_installs_only_selected_bytes_with_nonwritable_mode(self):
        archive, digest = self.archive()
        target = self.root / 'covenant-compute'
        MODULE.install_archive(archive, digest, target)
        self.assertEqual(target.read_bytes(), b'fixture executable bytes')
        self.assertEqual(target.stat().st_mode & 0o777, 0o555)

    def test_bad_digest_cannot_replace_existing_binary(self):
        archive, _ = self.archive()
        target = self.root / 'covenant-compute'
        target.write_bytes(b'old')
        with self.assertRaises(ValueError):
            MODULE.install_archive(archive, '0' * 64, target)
        self.assertEqual(target.read_bytes(), b'old')

    def test_traversal_absolute_symlink_hardlink_and_duplicate_refused(self):
        for name, kind in (('../escape', tarfile.REGTYPE), ('/absolute', tarfile.REGTYPE),
                           ('x/../escape', tarfile.REGTYPE), ('linked', tarfile.SYMTYPE),
                           ('hardlinked', tarfile.LNKTYPE), (MODULE.ASSET + '/covenant-compute', tarfile.REGTYPE)):
            with self.subTest(name=name, kind=kind):
                item = tarfile.TarInfo(name)
                item.type = kind
                item.linkname = '/etc/passwd'
                archive, digest = self.archive([item])
                target = self.root / 'target'
                with self.assertRaises(ValueError):
                    MODULE.install_archive(archive, digest, target)
                self.assertFalse(target.exists())
        self.assertFalse((self.root.parent / 'escape').exists())

    def test_missing_or_oversized_archive_refused(self):
        archive, digest = self.archive(executable=False)
        with self.assertRaises(ValueError):
            MODULE.install_archive(archive, digest, self.root / 'target')
        archive, digest = self.archive()
        with patch.object(MODULE, 'MAX_ARCHIVE', 1), self.assertRaises(ValueError):
            MODULE.install_archive(archive, digest, self.root / 'target')

    def test_redirects_are_tls_and_exact_allowlisted_hosts(self):
        for url in ('https://github.com/a', 'https://release-assets.githubusercontent.com/a?token=temporary'):
            self.assertTrue(MODULE.redirect_allowed(url))
        for url in ('http://github.com/a', 'https://github.com.evil.test/a', 'https://evil.test/a',
                    'https://user:pass@github.com/a', 'https://github.com:8443/a'):
            self.assertFalse(MODULE.redirect_allowed(url))

    def test_write_once_refuses_replacement_and_symlinks(self):
        target = self.root / 'identity'
        with patch.object(MODULE.os, 'chown'), patch.object(MODULE.os, 'fchown'):
            MODULE.write_once(target, b'a' * 32, self.owner)
            MODULE.write_once(target, b'a' * 32, self.owner)
            with self.assertRaises(ValueError):
                MODULE.write_once(target, b'b' * 32, self.owner)
            target.unlink()
            outside = self.root / 'outside'
            outside.write_bytes(b'a' * 32)
            target.symlink_to(outside)
            with self.assertRaises(ValueError):
                MODULE.write_once(target, b'a' * 32, self.owner)
            self.assertEqual(outside.read_bytes(), b'a' * 32)

    def test_first_download_caches_verified_archive_then_boots_without_network(self):
        archive, digest = self.archive()
        original = archive.read_bytes()
        response = io.BytesIO(original)
        response.status = 200
        response.headers = {}
        response.geturl = lambda: MODULE.URL
        opener = Mock()
        opener.open.return_value = response
        cache, binary = self.root / 'cache', self.root / 'bin'
        env = {'COVENANT_CANARY_RELEASE_SHA256': digest}
        with patch.object(MODULE, 'CACHE', cache), patch.object(MODULE, 'BIN', binary), \
                patch.object(MODULE.urllib.request, 'build_opener', return_value=opener) as network, \
                patch.object(MODULE, 'verify_client_version') as version:
            MODULE.install_client(env)
            saved = cache / (MODULE.VERSION + '-' + digest + '.tar.gz')
            self.assertEqual(saved.read_bytes(), original)
            self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
            self.assertEqual(cache.stat().st_mode & 0o777, 0o700)
            self.assertEqual((binary / 'covenant-compute').read_bytes(), b'fixture executable bytes')
            (binary / 'covenant-compute').unlink()
            network.reset_mock()
            network.side_effect = AssertionError('a cached restart must not contact GitHub')
            MODULE.install_client(env)
            network.assert_not_called()
            self.assertEqual((binary / 'covenant-compute').read_bytes(), b'fixture executable bytes')
            self.assertEqual(version.call_count, 2)

    def test_corrupt_cache_is_not_replaced_or_downloaded_and_cannot_replace_binary(self):
        _, digest = self.archive()
        cache, binary = self.root / 'cache', self.root / 'bin'
        cache.mkdir(mode=0o700)
        binary.mkdir()
        existing = binary / 'covenant-compute'
        existing.write_bytes(b'previous verified binary')
        saved = cache / (MODULE.VERSION + '-' + digest + '.tar.gz')
        saved.write_bytes(b'corrupt archive')
        with patch.object(MODULE, 'CACHE', cache), patch.object(MODULE, 'BIN', binary), \
                patch.object(MODULE.urllib.request, 'build_opener') as network, \
                patch.object(MODULE, 'verify_client_version') as version:
            with self.assertRaises(ValueError):
                MODULE.install_client({'COVENANT_CANARY_RELEASE_SHA256': digest})
            network.assert_not_called()
            version.assert_not_called()
        self.assertEqual(saved.read_bytes(), b'corrupt archive')
        self.assertEqual(existing.read_bytes(), b'previous verified binary')

    def test_cache_metadata_requires_effective_owner_private_write_regular_single_link(self):
        good = dict(st_uid=os.geteuid(), st_mode=stat.S_IFREG | 0o600, st_nlink=1, st_size=1024)
        for changes in ({'st_uid': os.geteuid() + 1}, {'st_mode': stat.S_IFREG | 0o620},
                        {'st_mode': stat.S_IFREG | 0o602}, {'st_mode': stat.S_IFLNK | 0o600},
                        {'st_mode': stat.S_IFIFO | 0o600}, {'st_nlink': 2}, {'st_size': 0},
                        {'st_size': MODULE.MAX_ARCHIVE + 1}):
            with self.subTest(changes=changes), \
                    patch.object(Path, 'lstat', return_value=types.SimpleNamespace(**{**good, **changes})), \
                    self.assertRaises(ValueError):
                MODULE.require_cache_metadata(self.root / 'archive')
        with patch.object(Path, 'lstat', return_value=types.SimpleNamespace(**good)):
            MODULE.require_cache_metadata(self.root / 'archive')
        directory = {**good, 'st_mode': stat.S_IFDIR | 0o700, 'st_nlink': 2}
        with patch.object(Path, 'lstat', return_value=types.SimpleNamespace(**directory)):
            MODULE.require_cache_metadata(self.root / 'cache', directory=True)
        for changes in ({'st_uid': os.geteuid() + 1}, {'st_mode': stat.S_IFDIR | 0o777},
                        {'st_mode': stat.S_IFREG | 0o700}, {'st_mode': stat.S_IFLNK | 0o700}):
            with self.subTest(directory_changes=changes), \
                    patch.object(Path, 'lstat', return_value=types.SimpleNamespace(**{**directory, **changes})), \
                    self.assertRaises(ValueError):
                MODULE.require_cache_metadata(self.root / 'cache', directory=True)

    def test_cache_symlink_directory_and_hardlinked_archive_refuse_before_network(self):
        archive, digest = self.archive()
        cache, binary = self.root / 'cache', self.root / 'bin'
        outside = self.root / 'outside'
        outside.mkdir()
        cache.symlink_to(outside)
        with patch.object(MODULE, 'CACHE', cache), patch.object(MODULE, 'BIN', binary), \
                patch.object(MODULE.urllib.request, 'build_opener') as network:
            with self.assertRaises(ValueError):
                MODULE.install_client({'COVENANT_CANARY_RELEASE_SHA256': digest})
            cache.unlink()
            cache.mkdir(mode=0o700)
            saved = cache / (MODULE.VERSION + '-' + digest + '.tar.gz')
            os.link(archive, saved)
            with self.assertRaises(ValueError):
                MODULE.install_client({'COVENANT_CANARY_RELEASE_SHA256': digest})
            network.assert_not_called()
        self.assertFalse((binary / 'covenant-compute').exists())


class EntrypointLayoutTests(TemporaryTests):
    def invoke(self, values):
        # Stub privileged commands: the real shell branches execute, but no
        # system directories, ownership, keys or service processes are touched.
        commands = self.root / 'commands'
        tools = self.root / 'tools'
        tools.mkdir(exist_ok=True)
        for name in ('mkdir', 'chown', 'chmod', 'python3', 'setpriv'):
            path = tools / name
            path.write_text('#!/bin/sh\nprintf "%s\\n" "${0##*/} $*" >> "$CANARY_TEST_COMMANDS"\n')
            path.chmod(0o700)
        commands.write_text('')
        entry = PATH.parent / 'compute-coordinator-entrypoint.sh'
        result = subprocess.run(['/bin/sh', str(entry)], text=True, capture_output=True,
            env={'PATH': str(tools) + ':/usr/bin:/bin', 'CANARY_TEST_COMMANDS': str(commands), **values}, timeout=5)
        return result, commands.read_text().splitlines()

    def test_opt_in_refuses_default_or_unrelated_coordinator_home(self):
        for home in (None, '/data', '/tmp/elsewhere', '/data/mainnet/'):
            env = {'COVENANT_CANARY_MODE': 'scheduled'}
            if home is not None:
                env['COVENANT_COMPUTE_COORDINATOR_HOME'] = home
            with self.subTest(home=home):
                result, commands = self.invoke(env)
                self.assertEqual(result.returncode, 1)
                self.assertIn('requires existing coordinator home /data/mainnet', result.stderr)
                self.assertFalse(any(command.startswith(('python3 ', 'setpriv ', 'chmod 755')) for command in commands))

    def test_opt_in_uses_traversable_root_and_private_existing_mainnet_home(self):
        result, commands = self.invoke({'COVENANT_CANARY_MODE': 'disabled',
                                       'COVENANT_COMPUTE_COORDINATOR_HOME': '/data/mainnet'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('chown root:root /data', commands)
        self.assertIn('chmod 755 /data', commands)
        self.assertIn('chmod 700 /data/mainnet', commands)
        self.assertEqual(commands[-1], 'python3 /opt/covenant/compute-canary-supervisor.py')
        self.assertFalse(any(command.startswith('setpriv ') for command in commands))

    def test_default_without_canary_keeps_legacy_entrypoint(self):
        result, commands = self.invoke({})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(commands[-1], 'setpriv --reuid=covenant --regid=covenant --init-groups -- covenant-compute-coordinator')
        self.assertFalse(any(command.startswith(('python3 ', 'chmod 755 /data', 'chmod 700 /data')) for command in commands))


class ScheduleTests(TemporaryTests):
    def test_once_and_preflight_markers_survive_restart(self):
        now = 20 * MODULE.WINDOW
        self.assertIsNone(MODULE.scheduling_action('once', {'once_attempted': True}, False, now))
        self.assertIsNone(MODULE.scheduling_action('preflight', {'preflight_attempted': True}, False, now))
        self.assertEqual(MODULE.scheduling_action('once', {}, False, now), 'rent')

    def test_scheduled_window_deduplication_and_clock_rollback(self):
        journal = {'last_window': 20}
        for now in (19 * MODULE.WINDOW, 20 * MODULE.WINDOW, 21 * MODULE.WINDOW - 1):
            self.assertIsNone(MODULE.scheduling_action('scheduled', journal, False, now))
        self.assertEqual(MODULE.scheduling_action('scheduled', journal, False, 21 * MODULE.WINDOW), 'rent')

    def test_disabled_or_preflight_still_recovers_but_never_new_rental(self):
        for mode in ('disabled', 'preflight', 'once', 'scheduled'):
            self.assertEqual(MODULE.scheduling_action(mode, {}, True, 1000), 'recover')
            self.assertIsNone(MODULE.scheduling_action(mode, {'recovery_attempt_at': 900}, True, 1000))
        self.assertIsNone(MODULE.scheduling_action('disabled', {}, False, 1000))

    def test_missing_corrupt_or_badly_typed_schedule_cannot_reset_pilot(self):
        state = self.root / 'state'
        state.mkdir()
        path = self.root / 'supervisor.json'
        self.assertEqual(MODULE.read_schedule(path, state), {'schema': 1})
        (state / 'budget-initialized').write_text('{}')
        with self.assertRaises(ValueError):
            MODULE.read_schedule(path, state)
        for content in ('broken', '{"schema":1,"once_attempted":"false"}', '{"schema":1,"last_window":-1}'):
            path.write_text(content)
            with self.assertRaises((ValueError, TypeError)):
                MODULE.read_schedule(path, state)
        path.write_text('{"schema":1,"once_attempted":true}')
        self.assertTrue(MODULE.read_schedule(path, state)['once_attempted'])

    def test_unknown_ledger_is_recovery_only(self):
        state = self.root / 'state'
        state.mkdir()
        self.assertFalse(MODULE.unresolved(state))
        (state / 'budget-initialized').write_text('{}')
        self.assertTrue(MODULE.unresolved(state))
        (state / 'budget.json').write_text('broken')
        self.assertTrue(MODULE.unresolved(state))
        (state / 'budget.json').write_text('{"entries":[{"resolved_at_unix":null}]}')
        self.assertTrue(MODULE.unresolved(state))
        (state / 'budget.json').write_text('{"entries":[{"resolved_at_unix":12}]}')
        self.assertFalse(MODULE.unresolved(state))

    def test_canary_environment_does_not_inherit_administrative_secrets(self):
        env = {'COVENANT_CANARY_MODE': 'scheduled', 'COVENANT_CANARY_BUYER_SEED_BASE64': 'never-in-child-env',
               'COVENANT_CANARY_SSH_PRIVATE_KEY_BASE64': 'never-in-child-env',
               'COVENANT_CANARY_VAST_API_KEY': 'read-only-provider', 'COVENANT_COMPUTE_ADMIN_KEY': 'admin-secret',
               'COVENANT_CANARY_ALERT_CLOUDFLARE_API_TOKEN': 'send-only-email', 'LD_PRELOAD': 'evil', 'PYTHONPATH': 'evil'}
        coordinator, canary = MODULE.environments(env)
        self.assertEqual(canary['COVENANT_CANARY_VAST_API_KEY'], 'read-only-provider')
        for key in ('COVENANT_CANARY_BUYER_SEED_BASE64', 'COVENANT_CANARY_SSH_PRIVATE_KEY_BASE64',
                    'COVENANT_COMPUTE_ADMIN_KEY', 'LD_PRELOAD', 'PYTHONPATH'):
            self.assertNotIn(key, canary)
        self.assertFalse(any(k.startswith('COVENANT_CANARY_') for k in coordinator))
        self.assertEqual(coordinator['COVENANT_COMPUTE_ADMIN_KEY'], 'admin-secret')

    def test_spawn_drops_uid_groups_and_inherited_stdio(self):
        with patch.object(MODULE.subprocess, 'Popen') as popen:
            MODULE.spawn(['python3', 'runner.py'], {'PATH': '/usr/bin'}, self.owner)
        args = popen.call_args.kwargs
        self.assertEqual(args['user'], self.owner.pw_uid)
        self.assertEqual(args['group'], self.owner.pw_gid)
        self.assertEqual(args['extra_groups'], [])
        self.assertTrue(args['start_new_session'])
        self.assertEqual(args['stdin'], MODULE.subprocess.DEVNULL)
        self.assertEqual(args['stdout'], MODULE.subprocess.DEVNULL)

    def test_bootstrap_verifies_buyer_and_ssh_public_key_as_unprivileged_user(self):
        (self.root / 'ssh').mkdir()
        (self.root / 'ssh/id_ed25519.pub').write_text('ssh-ed25519 fixture comment\n')
        env = {'COVENANT_CANARY_BUYER_PUBKEY': 'expected-public'}
        with patch.object(MODULE, 'ROOT', self.root), patch.object(MODULE.subprocess, 'check_output', side_effect=[b'{"pubkey":"expected-public"}', 'ssh-ed25519 fixture\n']) as output:
            MODULE.verify_identity(env, self.owner)
        for call in output.call_args_list:
            self.assertEqual(call.kwargs['user'], self.owner.pw_uid)
            self.assertEqual(call.kwargs['extra_groups'], [])
            self.assertEqual(set(call.kwargs['env']), {'PATH', 'COVENANT_COMPUTE_MCP_HOME'})
        with patch.object(MODULE, 'ROOT', self.root), patch.object(MODULE.subprocess, 'check_output', return_value=b'{"pubkey":"wrong"}'), self.assertRaises(ValueError):
            MODULE.verify_identity(env, self.owner)
        with patch.object(MODULE, 'ROOT', self.root), patch.object(MODULE.subprocess, 'check_output', side_effect=[b'{"pubkey":"expected-public"}', 'ssh-ed25519 wrong\n']), self.assertRaises(ValueError):
            MODULE.verify_identity(env, self.owner)


class LoopTests(TemporaryTests):
    def run_loop(self, mode, journal=None, ledger=None, corrupt_schedule=False, persistence_fails=False,
                 ready=True):
        state = self.root / 'state'
        state.mkdir(exist_ok=True)
        path = self.root / 'supervisor.json'
        if journal is not None:
            path.write_text(json.dumps(journal))
        if corrupt_schedule:
            path.write_text('corrupt; preserve me')
        if ledger is not None:
            (state / 'budget.json').write_text(json.dumps(ledger))
            (state / 'budget-initialized').write_text('{}')
        coordinator = types.SimpleNamespace(returncode=None, pid=123456)
        coordinator.poll = lambda: coordinator.returncode
        dispatched = []
        ticks = [0]
        original_atomic = MODULE.atomic
        def persist(*args, **kwargs):
            if persistence_fails:
                raise OSError('simulated full disk')
            return original_atomic(*args, **kwargs)
        def sleep(_):
            ticks[0] += 1
            if ticks[0] >= 3:
                coordinator.returncode = 0
        def spawn(argv, env, owner):
            # Every admission marker is already durable at child launch.
            if '--once' in argv:
                saved = json.loads(path.read_text())
                self.assertTrue(saved['once_attempted'])
                self.assertEqual(saved['last_window'], 20)
            dispatched.append(argv)
            return types.SimpleNamespace(poll=lambda: 0, returncode=0, pid=123457)
        readiness = Mock()
        if callable(ready):
            readiness.ready.side_effect = ready
        else:
            readiness.ready.return_value = ready
        self.readiness = readiness
        with patch.object(MODULE, 'ROOT', self.root), patch.dict(MODULE.os.environ, {'COVENANT_CANARY_MODE': mode}, clear=True), \
             patch.object(MODULE.pwd, 'getpwnam', return_value=self.owner), patch.object(MODULE.subprocess, 'Popen', return_value=coordinator), \
             patch.object(MODULE, 'install_client'), patch.object(MODULE, 'prepare'), patch.object(MODULE, 'verify_identity'), patch.object(MODULE, 'spawn', side_effect=spawn), \
             patch.object(MODULE.time, 'time', return_value=20 * MODULE.WINDOW), patch.object(MODULE.time, 'sleep', side_effect=sleep), \
             patch.object(MODULE.signal, 'signal'), patch.object(MODULE.os, 'fchown'), patch.object(MODULE.sys, 'argv', ['supervisor']), \
             patch.object(MODULE, 'atomic', side_effect=persist), \
             patch.object(MODULE, 'AdmissionReadiness', return_value=readiness), patch('builtins.print'):
            self.assertEqual(MODULE.main(), 0)
        return dispatched

    def test_restarting_in_once_mode_does_not_buy_again(self):
        calls = self.run_loop('once', journal={'schema': 1, 'once_attempted': True})
        self.assertFalse(any('--once' in call for call in calls))

    def test_scheduled_launch_marker_is_durable_before_spawn_and_not_repeated(self):
        calls = self.run_loop('scheduled')
        self.assertEqual(sum('--once' in call for call in calls), 1)

    def test_failed_admission_journal_write_prevents_dispatch(self):
        calls = self.run_loop('scheduled', persistence_fails=True)
        self.assertFalse(any('--once' in call for call in calls))
        self.assertFalse((self.root / 'supervisor.json').exists())

    def test_paused_pending_work_uses_recovery_only_flag(self):
        calls = self.run_loop('disabled', journal={'schema': 1}, ledger={'entries': [{'resolved_at_unix': None}]})
        self.assertEqual(sum('--recover-only' in call for call in calls), 1)
        self.assertFalse(any('--once' in call for call in calls))

    def test_lost_admission_journal_never_authorizes_another_once(self):
        calls = self.run_loop('once', ledger={'entries': [{'resolved_at_unix': 12}]})
        self.assertFalse(any('--once' in call for call in calls))

    def test_corrupt_admission_state_still_allows_recovery_without_overwriting_it(self):
        calls = self.run_loop('disabled', ledger={'entries': [{'resolved_at_unix': None}]}, corrupt_schedule=True)
        self.assertEqual(sum('--recover-only' in call for call in calls), 1)
        self.assertEqual((self.root / 'supervisor.json').read_text(), 'corrupt; preserve me')
        self.assertTrue((self.root / 'recovery-supervisor.json').exists())

    def test_unready_public_route_never_consumes_preflight_once_or_window(self):
        for mode in ('preflight', 'once', 'scheduled'):
            with self.subTest(mode=mode):
                calls = self.run_loop(mode, ready=False)
                self.assertFalse(any('--once' in call or '--preflight' in call for call in calls))
                self.assertFalse((self.root / 'supervisor.json').exists())
                self.assertTrue(any('--monitor' in call for call in calls))

    def test_later_readiness_admits_once_with_marker_durable_before_spawn(self):
        observations = iter([False, False, True])
        def ready():
            self.assertFalse((self.root / 'supervisor.json').exists())
            return next(observations)
        calls = self.run_loop('once', ready=ready)
        self.assertEqual(sum('--once' in call for call in calls), 1)
        self.assertEqual(self.readiness.ready.call_count, 3)

    def test_public_readiness_is_not_consulted_for_recovery(self):
        calls = self.run_loop('once', journal={'schema': 1},
                              ledger={'entries': [{'resolved_at_unix': None}]}, ready=False)
        self.assertEqual(sum('--recover-only' in call for call in calls), 1)
        self.assertTrue(any('--monitor' in call for call in calls))
        self.readiness.ready.assert_not_called()


class ReadinessTests(unittest.TestCase):
    def response(self, value=None, **changes):
        data = json.dumps(value or {'status': 'ok', 'revision': 'a' * 40}).encode()
        response = io.BytesIO(data)
        response.status = changes.get('status', 200)
        response.headers = changes.get('headers', {'Content-Type': 'application/json', 'Content-Length': str(len(data))})
        response.geturl = lambda: changes.get('url', MODULE.PUBLIC_HEALTH)
        return response

    def read(self, response):
        opener = Mock()
        opener.open.return_value = response
        with patch.object(MODULE.urllib.request, 'build_opener', return_value=opener) as build, \
                patch.object(MODULE.signal, 'signal'), patch.object(MODULE.signal, 'setitimer') as timer:
            value = MODULE.health_revision(MODULE.PUBLIC_HEALTH)
        request = opener.open.call_args.args[0]
        self.assertEqual(request.get_header('User-agent'), 'Covenant-Rental-Canary/0.3.1')
        self.assertEqual(request.get_header('Accept-encoding'), 'identity')
        self.assertNotIn('Authorization', request.headers)
        self.assertEqual(opener.open.call_args.kwargs['timeout'], MODULE.HEALTH_TIMEOUT)
        self.assertEqual(build.call_args.args[0].proxies, {})
        self.assertIsInstance(build.call_args.args[1], MODULE.NoHealthRedirect)
        self.assertEqual(timer.call_args_list[-1].args, (MODULE.signal.ITIMER_REAL, 0))
        return value

    def test_health_checks_use_fixed_public_origin_custom_agent_and_no_proxies(self):
        self.assertEqual(self.read(self.response()), 'a' * 40)
        self.assertIsNone(MODULE.NoHealthRedirect().redirect_request(None, None, 302, None, None, 'https://elsewhere.test'))
        with self.assertRaises(ValueError):
            MODULE.health_revision('https://compute-api.opencovenant.org.evil.test/health')

    def test_local_port_is_same_validated_coordinator_setting(self):
        self.assertEqual(MODULE.local_health_url({}), 'http://127.0.0.1:8720/health')
        self.assertEqual(MODULE.local_health_url({'COVENANT_COMPUTE_COORDINATOR_PORT': '10000'}), 'http://127.0.0.1:10000/health')
        for port in ('0', '65536', '-1', '8720/path', '127.0.0.1:8720', ' 8720', ''):
            with self.subTest(port=port), self.assertRaises(ValueError):
                MODULE.local_health_url({'COVENANT_COMPUTE_COORDINATOR_PORT': port})

    def test_bad_status_revision_encoding_length_or_redirect_fails(self):
        cases = [self.response(status=503), self.response({'status': 'degraded', 'revision': 'a' * 40}),
                 self.response({'status': 'ok', 'revision': 'development'}), self.response(url='https://elsewhere.test/health'),
                 self.response(headers={'Content-Type': 'text/html'}),
                 self.response(headers={'Content-Type': 'application/json', 'Content-Encoding': 'gzip'}),
                 self.response(headers={'Content-Type': 'application/json', 'Content-Length': '4097'}),
                 self.response(headers={'Content-Type': 'application/json', 'Content-Length': '1'}),
                 self.response({'status': 'ok', 'revision': 'a' * 40, 'padding': 'x' * 4096})]
        for response in cases:
            with self.subTest(response=response), self.assertRaises(ValueError):
                self.read(response)

    def test_total_deadline_is_cleared_after_failure(self):
        opener = Mock()
        opener.open.side_effect = TimeoutError()
        with patch.object(MODULE.urllib.request, 'build_opener', return_value=opener), \
                patch.object(MODULE.signal, 'signal'), patch.object(MODULE.signal, 'setitimer') as timer, \
                self.assertRaises(TimeoutError):
            MODULE.health_revision(MODULE.PUBLIC_HEALTH)
        self.assertEqual(timer.call_args_list[-1].args, (MODULE.signal.ITIMER_REAL, 0))

    def test_two_matching_observations_at_least_five_seconds_apart(self):
        gate = MODULE.AdmissionReadiness({})
        with patch.object(MODULE.time, 'monotonic', side_effect=[0, 1, 4, 5, 6]), \
                patch.object(MODULE, 'health_revision', return_value='a' * 40) as health:
            self.assertEqual([gate.ready() for _ in range(5)], [False, False, False, True, True])
        self.assertEqual(health.call_count, 4)
        self.assertEqual(health.call_args_list[0].args, ('http://127.0.0.1:8720/health',))
        self.assertEqual(health.call_args_list[1].args, (MODULE.PUBLIC_HEALTH,))

    def test_unavailable_or_mismatched_routes_reset_consecutive_observations(self):
        for failure in (TimeoutError(), 'b' * 40):
            gate = MODULE.AdmissionReadiness({})
            with self.subTest(failure=failure), \
                    patch.object(MODULE.time, 'monotonic', side_effect=[0, 5, 10, 15]), \
                    patch.object(MODULE, 'health_revision', side_effect=['a' * 40, 'a' * 40, 'a' * 40, failure,
                                                                      'a' * 40, 'a' * 40, 'a' * 40, 'a' * 40]):
                self.assertEqual([gate.ready() for _ in range(4)], [False, False, False, True])

    def test_previous_window_readiness_cannot_authorize_later_window(self):
        gate = MODULE.AdmissionReadiness({})
        with patch.object(MODULE.time, 'monotonic', side_effect=[0, 5, 21600, 21605]), \
                patch.object(MODULE, 'health_revision', return_value='a' * 40):
            self.assertEqual([gate.ready() for _ in range(4)], [False, True, False, True])


if __name__ == '__main__':
    unittest.main()
